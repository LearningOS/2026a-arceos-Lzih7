#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]
#![feature(asm_const)]
#![feature(riscv_ext_intrinsics)]

extern crate alloc;
#[cfg(feature = "axstd")]
extern crate axstd as std;
#[macro_use]
extern crate axlog;

mod csrs;
mod loader;
mod regs;
mod sbi;
mod task;
mod vcpu;

use crate::regs::GprIndex::{self, A0, A1};
use alloc::boxed::Box;
use axhal::mem::{phys_to_virt, virt_to_phys, PAGE_SIZE_4K};
use csrs::defs::hstatus;
use csrs::{RiscvCsrTrait, CSR};
use loader::load_vm_image;
use riscv::register::{htinst, htval, scause, sstatus, stval};
use sbi::SbiMessage;
use tock_registers::LocalRegisterCopy;
use vcpu::VmCpuRegisters;
use vcpu::_run_guest;

const VM_ENTRY: usize = 0x8020_0000;
const VM_ASPACE_SIZE: usize = 0x40_0000_0000;
const VM_HART_ID: usize = 0x1234;
const VM_MMIO_ADDR: usize = 0x40;
const VM_MMIO_VALUE: usize = 0x6688;

// Sv39x4 requires four root pages, aligned to 16 KiB.
#[repr(C, align(16384))]
struct GuestPageTableRoot([u64; 2048]);

#[cfg_attr(feature = "axstd", no_mangle)]
fn main() {
    ax_println!("Hypervisor ...");

    // Here AddrSpace's virtual addresses represent guest physical addresses.
    // The guest does not need the host kernel's mappings.
    let mut uspace = axmm::AddrSpace::new_empty(0.into(), VM_ASPACE_SIZE).unwrap();

    // Load vm binary file into address space.
    if let Err(e) = load_vm_image("/sbin/skernel2", &mut uspace) {
        panic!("Cannot load app! {:?}", e);
    }

    // Setup context to prepare to enter guest mode.
    let mut ctx = VmCpuRegisters::default();
    prepare_guest_context(&mut ctx);

    // Setup pagetable for 2nd address mapping.
    let _ept_root = prepare_vm_pgtable(&uspace);

    // Kick off vm and wait for it to exit.
    while !run_guest(&mut ctx, &uspace) {}

    // Stop using the guest tables before their storage is released.
    unsafe {
        core::arch::asm!("csrw hgatp, zero");
        core::arch::riscv64::hfence_gvma_all();
    }
    ax_println!("Hypervisor ok!");
}

fn prepare_vm_pgtable(aspace: &axmm::AddrSpace) -> Box<GuestPageTableRoot> {
    let mut root = Box::new(GuestPageTableRoot([0; 2048]));
    // The guest uses the low Sv39 range, so the first 512 entries can share
    // AddrSpace's lower-level tables. The other three root pages stay zeroed.
    unsafe {
        core::ptr::copy_nonoverlapping(
            phys_to_virt(aspace.page_table_root())
                .as_ptr()
                .cast::<u64>(),
            root.0.as_mut_ptr(),
            PAGE_SIZE_4K / core::mem::size_of::<u64>(),
        );
    }
    let root_paddr = virt_to_phys((root.0.as_ptr() as usize).into());
    let hgatp = 8usize << 60 | root_paddr.as_usize() >> 12;
    unsafe {
        core::arch::asm!(
            "csrw hgatp, {hgatp}",
            hgatp = in(reg) hgatp,
        );
        core::arch::riscv64::hfence_gvma_all();
    }
    root
}

fn run_guest(ctx: &mut VmCpuRegisters, aspace: &axmm::AddrSpace) -> bool {
    // The assembly changes stvec, sscratch and sp in separate instructions.
    // Mask HS interrupts even while V=1, and restore them only on the host stack.
    let irq_state = axhal::arch::local_irq_save_and_disable();
    let sie = CSR.sie.atomic_replace(0);
    unsafe {
        _run_guest(ctx);
    }

    ctx.trap_csrs.scause = scause::read().bits();
    ctx.trap_csrs.stval = stval::read();
    ctx.trap_csrs.htval = htval::read();
    ctx.trap_csrs.htinst = htinst::read();
    let finished = vmexit_handler(ctx, aspace);
    CSR.sie.write_value(sie);
    axhal::arch::local_irq_restore(irq_state);
    finished
}

fn vmexit_handler(ctx: &mut VmCpuRegisters, aspace: &axmm::AddrSpace) -> bool {
    use scause::{Exception, Trap};

    let scause = scause::read();
    match scause.cause() {
        Trap::Exception(Exception::VirtualSupervisorEnvCall) => {
            let sbi_msg = SbiMessage::from_regs(ctx.guest_regs.gprs.a_regs()).ok();
            ax_println!("VmExit Reason: VSuperEcall: {:?}", sbi_msg);
            if let Some(msg) = sbi_msg {
                match msg {
                    SbiMessage::Reset(_) => {
                        let a0 = ctx.guest_regs.gprs.reg(A0);
                        let a1 = ctx.guest_regs.gprs.reg(A1);
                        ax_println!("a0 = {:#x}, a1 = {:#x}", a0, a1);
                        assert_eq!(a0, 0x6688);
                        assert_eq!(a1, 0x1234);
                        ax_println!("Shutdown vm normally!");
                        return true;
                    }
                    _ => panic!("Unsupported SBI message: {:?}", msg),
                }
            } else {
                panic!("bad sbi message! ");
            }
        }
        Trap::Exception(Exception::IllegalInstruction) => {
            let inst = guest_instruction(ctx, aspace);
            // csrr rd, mhartid is CSRRS rd, 0xf14, x0. Only rd may vary.
            assert_eq!(inst & !0xf80, 0xf1402073, "Unsupported CSR instruction");
            let rd = GprIndex::from_raw((inst >> 7) & 0x1f).unwrap();
            ctx.guest_regs.gprs.set_reg(rd, VM_HART_ID);
            ctx.guest_regs.sepc += 4;
            ax_println!("Emulate mhartid: {:?} = {:#x}", rd, VM_HART_ID);
        }
        Trap::Exception(Exception::LoadGuestPageFault) => {
            let fault_addr = (ctx.trap_csrs.htval << 2) | (ctx.trap_csrs.stval & 0x3);
            assert_eq!(
                fault_addr, VM_MMIO_ADDR,
                "Unexpected guest physical address"
            );
            let inst = guest_instruction(ctx, aspace);
            // Emulate the test device's 64-bit read: ld rd, 64(x0).
            assert_eq!(inst & !0xf80, 0x04003003, "Unsupported MMIO load");
            let rd = GprIndex::from_raw((inst >> 7) & 0x1f).unwrap();
            ctx.guest_regs.gprs.set_reg(rd, VM_MMIO_VALUE);
            ctx.guest_regs.sepc += 4;
            ax_println!(
                "Emulate MMIO read: [{:#x}] = {:#x}",
                fault_addr,
                VM_MMIO_VALUE
            );
        }
        _ => {
            panic!(
                "Unhandled trap: {:?}, sepc: {:#x}, stval: {:#x}",
                scause.cause(),
                ctx.guest_regs.sepc,
                ctx.trap_csrs.stval
            );
        }
    }
    false
}

fn guest_instruction(ctx: &VmCpuRegisters, aspace: &axmm::AddrSpace) -> u32 {
    // This payload keeps vsatp in Bare mode, so its instruction VA equals GPA.
    let mut bytes = [0; 4];
    aspace
        .read(ctx.guest_regs.sepc.into(), &mut bytes)
        .expect("Cannot read guest instruction");
    u32::from_le_bytes(bytes)
}

fn prepare_guest_context(ctx: &mut VmCpuRegisters) {
    // Set hstatus
    let mut hstatus =
        LocalRegisterCopy::<usize, hstatus::Register>::new(riscv::register::hstatus::read().bits());
    // Set Guest bit in order to return to guest mode.
    hstatus.modify(hstatus::spv::Guest);
    // Set SPVP bit in order to accessing VS-mode memory from HS-mode.
    hstatus.modify(hstatus::spvp::Supervisor);
    CSR.hstatus.write_value(hstatus.get());
    ctx.guest_regs.hstatus = hstatus.get();

    // Set sstatus in guest mode.
    let mut sstatus = sstatus::read();
    sstatus.set_spp(sstatus::SPP::Supervisor);
    // Never enable host interrupts partway through the assembly switch.
    ctx.guest_regs.sstatus = sstatus.bits() & !(1 << 1);
    // Return to entry to start vm.
    ctx.guest_regs.sepc = VM_ENTRY;
}
