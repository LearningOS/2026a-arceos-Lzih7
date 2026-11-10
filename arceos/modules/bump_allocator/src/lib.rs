#![no_std]

use allocator::{AllocError, AllocResult, BaseAllocator, ByteAllocator, PageAllocator};
use core::{alloc::Layout, ptr::NonNull};

/// Early memory allocator
/// Use it before formal bytes-allocator and pages-allocator can work!
/// This is a double-end memory range:
/// - Alloc bytes forward
/// - Alloc pages backward
///
/// [ bytes-used | avail-area | pages-used ]
/// |            | -->    <-- |            |
/// start      byte_pos    page_pos      end
///
/// For bytes area, 'allocations' records the number of live allocations.
/// When it goes down to ZERO, free bytes-used area.
/// For pages area, it will never be freed!
///
pub struct EarlyAllocator<const SIZE: usize> {
    start: usize,
    end: usize,
    byte_pos: usize,
    page_pos: usize,
    allocations: usize,
}

impl<const SIZE: usize> EarlyAllocator<SIZE> {
    pub const fn new() -> Self {
        Self {
            start: 0,
            end: 0,
            byte_pos: 0,
            page_pos: 0,
            allocations: 0,
        }
    }
}

impl<const SIZE: usize> BaseAllocator for EarlyAllocator<SIZE> {
    fn init(&mut self, start: usize, size: usize) {
        assert!(SIZE.is_power_of_two(), "invalid page size");
        let end = start.checked_add(size).expect("memory region overflow");
        // Keep the high boundary page-aligned for backward page allocation.
        let end = end & !(SIZE - 1);
        assert!(start != 0 && start < end, "invalid memory region");
        self.start = start;
        self.end = end;
        self.byte_pos = start;
        self.page_pos = end;
        self.allocations = 0;
    }

    fn add_memory(&mut self, start: usize, size: usize) -> AllocResult {
        if !SIZE.is_power_of_two() || start == 0 || size == 0 {
            return Err(AllocError::InvalidParam);
        }
        let end = start.checked_add(size).ok_or(AllocError::InvalidParam)?;
        if self.start == self.end {
            if end & !(SIZE - 1) <= start {
                return Err(AllocError::InvalidParam);
            }
            self.init(start, size);
            return Ok(());
        }
        if start < self.end && end > self.start {
            return Err(AllocError::MemoryOverlap);
        }
        // A single pair of cursors cannot represent disjoint free regions or
        // grow through existing allocations. Only extend an unused boundary.
        if start == self.end && self.page_pos == self.end {
            let end = end & !(SIZE - 1);
            if end == self.end {
                return Err(AllocError::InvalidParam);
            }
            self.end = end;
            self.page_pos = end;
        } else if end == self.start && self.allocations == 0 {
            self.start = start;
            self.byte_pos = start;
        } else {
            return Err(AllocError::InvalidParam);
        }
        Ok(())
    }
}

impl<const SIZE: usize> ByteAllocator for EarlyAllocator<SIZE> {
    fn alloc(&mut self, layout: Layout) -> AllocResult<NonNull<u8>> {
        if layout.size() == 0 {
            return Err(AllocError::InvalidParam);
        }
        let aligned = self
            .byte_pos
            .checked_add(layout.align() - 1)
            .ok_or(AllocError::NoMemory)?
            & !(layout.align() - 1);
        let end = aligned
            .checked_add(layout.size())
            .ok_or(AllocError::NoMemory)?;
        if end > self.page_pos {
            return Err(AllocError::NoMemory);
        }
        let ptr = NonNull::new(aligned as *mut u8).ok_or(AllocError::NoMemory)?;
        self.byte_pos = end;
        self.allocations += 1;
        Ok(ptr)
    }

    fn dealloc(&mut self, _pos: NonNull<u8>, _layout: Layout) {
        // Callers must free each live allocation exactly once. Individual
        // holes are not reused while any byte allocation is still alive.
        assert!(self.allocations > 0, "no live byte allocations");
        self.allocations -= 1;
        if self.allocations == 0 {
            self.byte_pos = self.start;
        }
    }

    fn total_bytes(&self) -> usize {
        self.end - self.start
    }

    fn used_bytes(&self) -> usize {
        self.byte_pos - self.start
    }

    fn available_bytes(&self) -> usize {
        self.page_pos - self.byte_pos
    }
}

impl<const SIZE: usize> PageAllocator for EarlyAllocator<SIZE> {
    const PAGE_SIZE: usize = SIZE;

    fn alloc_pages(&mut self, num_pages: usize, align_pow2: usize) -> AllocResult<usize> {
        if num_pages == 0 || !align_pow2.is_power_of_two() || !SIZE.is_power_of_two() {
            return Err(AllocError::InvalidParam);
        }
        let size = num_pages
            .checked_mul(SIZE)
            .ok_or(AllocError::InvalidParam)?;
        let align = align_pow2.max(SIZE);
        let start = self
            .page_pos
            .checked_sub(size)
            .ok_or(AllocError::NoMemory)?
            & !(align - 1);
        if start < self.byte_pos || start == 0 {
            return Err(AllocError::NoMemory);
        }
        self.page_pos = start;
        Ok(start)
    }

    fn dealloc_pages(&mut self, _pos: usize, _num_pages: usize) {
        // This early allocator deliberately never reuses allocated pages.
    }

    fn total_pages(&self) -> usize {
        (self.end - self.start) / SIZE
    }

    fn used_pages(&self) -> usize {
        (self.end - self.page_pos) / SIZE
    }

    fn available_pages(&self) -> usize {
        (self.page_pos - self.byte_pos) / SIZE
    }
}

#[cfg(test)]
mod tests;
