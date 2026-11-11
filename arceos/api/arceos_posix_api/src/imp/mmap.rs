//! Eager private mappings for a caller-provided user address space.

use axerrno::LinuxError;
use axhal::paging::MappingFlags;

bitflags::bitflags! {
    #[derive(Debug)]
    /// permissions for sys_mmap
    ///
    /// See <https://github.com/bminor/glibc/blob/master/bits/mman.h>
    struct MmapProt: i32 {
        /// Page can be read.
        const PROT_READ = 1 << 0;
        /// Page can be written.
        const PROT_WRITE = 1 << 1;
        /// Page can be executed.
        const PROT_EXEC = 1 << 2;
    }
}

impl From<MmapProt> for MappingFlags {
    fn from(value: MmapProt) -> Self {
        let mut flags = MappingFlags::USER;
        if value.contains(MmapProt::PROT_READ) {
            flags |= MappingFlags::READ;
        }
        if value.contains(MmapProt::PROT_WRITE) {
            flags |= MappingFlags::WRITE;
        }
        if value.contains(MmapProt::PROT_EXEC) {
            flags |= MappingFlags::EXECUTE;
        }
        flags
    }
}

bitflags::bitflags! {
    #[derive(Debug)]
    /// flags for sys_mmap
    ///
    /// See <https://github.com/bminor/glibc/blob/master/bits/mman.h>
    struct MmapFlags: i32 {
        /// Share changes
        const MAP_SHARED = 1 << 0;
        /// Changes private; copy pages on write.
        const MAP_PRIVATE = 1 << 1;
        /// Map address must be exactly as requested, no matter whether it is available.
        const MAP_FIXED = 1 << 4;
        /// Don't use a file.
        const MAP_ANONYMOUS = 1 << 5;
        /// Don't check for reservations.
        const MAP_NORESERVE = 1 << 14;
        /// Allocation is for a stack.
        const MAP_STACK = 0x20000;
    }
}

/// Maps private file contents or zeroed anonymous pages into `aspace`.
///
/// The caller supplies and exclusively locks the current user address space.
/// `addr` is a hint, not a pointer dereferenced by this function. Returns a user
/// virtual address on success or a negative Linux errno on failure. Shared and
/// fixed mappings, PROT_NONE and write-only permissions are unsupported.
pub fn sys_mmap(
    aspace: &mut axmm::AddrSpace,
    addr: *mut usize,
    length: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    offset: isize,
) -> isize {
    use memory_addr::{VirtAddr, VirtAddrRange, PAGE_SIZE_4K};

    syscall_body!(sys_mmap, {
        let prot = MmapProt::from_bits(prot).ok_or(LinuxError::EINVAL)?;
        let flags = MmapFlags::from_bits(flags).ok_or(LinuxError::EINVAL)?;
        // Eager private mappings only: no shared writeback or fixed replacement.
        let supported = MmapFlags::MAP_PRIVATE | MmapFlags::MAP_ANONYMOUS;
        if !flags.contains(MmapFlags::MAP_PRIVATE) || flags.bits() & !supported.bits() != 0 {
            return Err(LinuxError::EINVAL);
        }
        if length == 0 || offset < 0 || offset as usize % PAGE_SIZE_4K != 0 {
            return Err(LinuxError::EINVAL);
        }
        // Sv39 cannot encode a writable, non-readable leaf. PROT_NONE is not
        // supported by this eager mapping backend either.
        if prot.is_empty()
            || (prot.contains(MmapProt::PROT_WRITE) && !prot.contains(MmapProt::PROT_READ))
        {
            return Err(LinuxError::EINVAL);
        }
        let size = length
            .checked_add(PAGE_SIZE_4K - 1)
            .ok_or(LinuxError::ENOMEM)? & !(PAGE_SIZE_4K - 1);
        (offset as usize).checked_add(size).ok_or(LinuxError::EINVAL)?;
        let file = if flags.contains(MmapFlags::MAP_ANONYMOUS) {
            None
        } else {
            let file = super::fd_ops::get_file_like(fd)?;
            if file.stat()?.st_mode & 0o170000 != 0o100000 {
                return Err(LinuxError::ENODEV);
            }
            Some(file)
        };

        // Keep the null page unmapped. addr is a hint, not MAP_FIXED.
        let base = aspace.base().as_usize().max(PAGE_SIZE_4K);
        let end = aspace.end().as_usize();
        if size > end - base {
            return Err(LinuxError::ENOMEM);
        }
        let limit = VirtAddrRange::from_start_size(VirtAddr::from(base), end - base);
        let hint = addr as usize & !(PAGE_SIZE_4K - 1);
        let start = if hint >= base && hint < end {
            aspace.find_free_area(hint.into(), size, limit)
                .or_else(|| aspace.find_free_area(base.into(), size, limit))
        } else {
            aspace.find_free_area(base.into(), size, limit)
        }
        .ok_or(LinuxError::ENOMEM)?;

        // Fill through the kernel's direct mapping via AddrSpace::write;
        // user PTEs already have their final permissions.
        aspace
            .map_alloc(start, size, prot.into(), true)
            .map_err(|_| LinuxError::ENOMEM)?;
        let loaded = (|| -> axerrno::LinuxResult<()> {
            if let Some(file) = file {
                let mut buf = [0u8; PAGE_SIZE_4K];
                let mut copied = 0;
                while copied < length {
                    let count = (length - copied).min(buf.len());
                    let n = file.read_at(offset as u64 + copied as u64, &mut buf[..count])?;
                    if n == 0 {
                        break; // Allocated pages are zeroed, including the EOF tail.
                    }
                    aspace
                        .write(start + copied, &buf[..n])
                        .map_err(|_| LinuxError::EFAULT)?;
                    copied += n;
                }
            }
            Ok(())
        })();
        if let Err(err) = loaded {
            let _ = aspace.unmap_alloc(start, size);
            return Err(err);
        }
        Ok(start.as_usize())
    })
}
