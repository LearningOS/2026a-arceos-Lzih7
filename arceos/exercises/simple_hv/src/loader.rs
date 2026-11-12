use crate::VM_ENTRY;
use axhal::mem::PAGE_SIZE_4K;
use axhal::paging::MappingFlags;
use axmm::AddrSpace;
use std::fs::File;
use std::io::{self, Read};

pub fn load_vm_image(fname: &str, uspace: &mut AddrSpace) -> io::Result<()> {
    let mut buf = [0u8; PAGE_SIZE_4K];
    let size = load_file(fname, &mut buf)?;

    uspace.map_alloc(
        VM_ENTRY.into(),
        PAGE_SIZE_4K,
        MappingFlags::READ | MappingFlags::EXECUTE | MappingFlags::USER,
        true,
    )?;

    let (paddr, _, _) = uspace
        .page_table()
        .query(VM_ENTRY.into())
        .unwrap_or_else(|_| panic!("Mapping failed for segment: {:#x}", VM_ENTRY));

    ax_println!("paddr: {:#x}", paddr);

    uspace.write(VM_ENTRY.into(), &buf[..size])?;

    Ok(())
}

fn load_file(fname: &str, buf: &mut [u8]) -> io::Result<usize> {
    ax_println!("app: {}", fname);
    let mut file = File::open(fname)?;
    let size = file.metadata()?.len();
    if size == 0 || size > buf.len() as u64 {
        return Err(axerrno::AxError::InvalidInput);
    }
    let size = size as usize;
    file.read_exact(&mut buf[..size])?;
    Ok(size)
}
