use super::*;

const PAGE: usize = 4096;

#[repr(align(4096))]
struct Pool([u8; 8 * PAGE]);

#[test]
fn bytes_are_aligned_and_preserved_until_all_are_freed() {
    let mut pool = Pool([0; 8 * PAGE]);
    let start = pool.0.as_mut_ptr() as usize;
    let mut alloc = EarlyAllocator::<PAGE>::new();
    alloc.init(start + 1, pool.0.len() - 1);

    let small = Layout::from_size_align(17, 16).unwrap();
    let large = Layout::from_size_align(100, 256).unwrap();
    let a = alloc.alloc(small).unwrap();
    let b = alloc.alloc(large).unwrap();
    assert_eq!(a.as_ptr() as usize % 16, 0);
    assert_eq!(b.as_ptr() as usize % 256, 0);
    assert!(a.as_ptr() as usize + small.size() <= b.as_ptr() as usize);
    unsafe { b.as_ptr().write_bytes(0x5a, large.size()) };

    alloc.dealloc(a, small);
    let c = alloc.alloc(small).unwrap();
    assert!(c.as_ptr() as usize >= b.as_ptr() as usize + large.size());
    assert!(
        unsafe { core::slice::from_raw_parts(b.as_ptr(), large.size()) }
            .iter()
            .all(|&byte| byte == 0x5a)
    );
    alloc.dealloc(b, large);
    alloc.dealloc(c, small);
    assert_eq!(alloc.used_bytes(), 0);
    assert_eq!(alloc.alloc(small).unwrap(), a);
}

#[test]
fn byte_and_page_allocations_cannot_overlap() {
    let mut pool = Pool([0; 8 * PAGE]);
    let start = pool.0.as_mut_ptr() as usize;
    let mut alloc = EarlyAllocator::<PAGE>::new();
    alloc.init(start, pool.0.len());
    let layout = Layout::from_size_align(3 * PAGE, PAGE).unwrap();
    let bytes = alloc.alloc(layout).unwrap();
    let pages = alloc.alloc_pages(5, PAGE).unwrap();
    assert_eq!(bytes.as_ptr() as usize + layout.size(), pages);
    assert_eq!(alloc.available_bytes(), 0);
    assert_eq!(alloc.available_pages(), 0);
    assert!(matches!(alloc.alloc(layout), Err(AllocError::NoMemory)));
    assert!(matches!(
        alloc.alloc_pages(1, PAGE),
        Err(AllocError::NoMemory)
    ));

    alloc.dealloc(bytes, layout);
    assert_eq!(alloc.available_bytes(), 3 * PAGE);
    assert_eq!(alloc.used_pages(), 5);
    let reused = alloc.alloc(layout).unwrap();
    assert_eq!(reused, bytes);
    assert!(reused.as_ptr() as usize + layout.size() <= pages);
}

#[test]
fn page_alignment_padding_is_reserved_and_pages_are_not_recycled() {
    let mut pool = Pool([0; 8 * PAGE]);
    let start = pool.0.as_mut_ptr() as usize;
    let mut alloc = EarlyAllocator::<PAGE>::new();
    alloc.init(start, pool.0.len());
    let page = alloc.alloc_pages(1, 4 * PAGE).unwrap();
    assert_eq!(page % (4 * PAGE), 0);
    assert!(page >= start && page + PAGE <= start + pool.0.len());
    assert_eq!(alloc.available_bytes(), page - start);
    let used = alloc.used_pages();
    alloc.dealloc_pages(page, 1);
    assert_eq!(alloc.used_pages(), used);
    let next = alloc.alloc_pages(1, 1).unwrap();
    assert_eq!(next % PAGE, 0);
    assert!(next + PAGE <= page);
}

#[test]
fn invalid_and_overflowing_requests_do_not_consume_memory() {
    let mut pool = Pool([0; 8 * PAGE]);
    let mut alloc = EarlyAllocator::<PAGE>::new();
    let layout = Layout::from_size_align(1, 1).unwrap();
    assert!(matches!(alloc.alloc(layout), Err(AllocError::NoMemory)));
    alloc.init(pool.0.as_mut_ptr() as usize, pool.0.len());
    let available = alloc.available_bytes();
    assert!(matches!(
        alloc.alloc(Layout::new::<()>()),
        Err(AllocError::InvalidParam)
    ));
    for (count, align) in [(0, PAGE), (1, 0), (1, 3), (usize::MAX, PAGE)] {
        assert!(matches!(
            alloc.alloc_pages(count, align),
            Err(AllocError::InvalidParam)
        ));
    }
    let huge = Layout::from_size_align(isize::MAX as usize, 1).unwrap();
    assert!(matches!(alloc.alloc(huge), Err(AllocError::NoMemory)));
    assert_eq!(alloc.available_bytes(), available);
    assert_eq!(alloc.used_bytes(), 0);
    assert_eq!(alloc.used_pages(), 0);
}

#[test]
fn adding_memory_extends_only_contiguous_unused_boundaries() {
    let mut pool = Pool([0; 8 * PAGE]);
    let start = pool.0.as_mut_ptr() as usize;
    let mut alloc = EarlyAllocator::<PAGE>::new();
    alloc.add_memory(start + 2 * PAGE, 2 * PAGE).unwrap();
    let layout = Layout::from_size_align(PAGE, PAGE).unwrap();
    let bytes = alloc.alloc(layout).unwrap();
    alloc.add_memory(start + 4 * PAGE, 2 * PAGE).unwrap();
    assert_eq!(alloc.total_bytes(), 4 * PAGE);
    assert!(matches!(
        alloc.add_memory(start, 2 * PAGE),
        Err(AllocError::InvalidParam)
    ));
    alloc.dealloc(bytes, layout);
    alloc.add_memory(start, 2 * PAGE).unwrap();
    assert_eq!(alloc.total_bytes(), 6 * PAGE);
    assert_eq!(alloc.alloc(layout).unwrap().as_ptr() as usize, start);

    alloc.alloc_pages(1, PAGE).unwrap();
    let available = alloc.available_bytes();
    assert!(matches!(
        alloc.add_memory(start + 6 * PAGE, PAGE),
        Err(AllocError::InvalidParam)
    ));
    assert!(matches!(
        alloc.add_memory(start + PAGE, PAGE),
        Err(AllocError::MemoryOverlap)
    ));
    assert!(matches!(
        alloc.add_memory(start + 7 * PAGE, PAGE),
        Err(AllocError::InvalidParam)
    ));
    assert!(matches!(
        alloc.add_memory(usize::MAX, PAGE),
        Err(AllocError::InvalidParam)
    ));
    assert_eq!(alloc.available_bytes(), available);
}
