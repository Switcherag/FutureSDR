//! Reuse of the mappings of dropped buffers.
//!
//! The pool is shared by the whole process, so everything is checked in one
//! test, in this test binary of its own.

use vmcircbuffer::double_mapped_buffer::DEFAULT_POOL_LIMIT;
use vmcircbuffer::double_mapped_buffer::DoubleMappedBuffer;
use vmcircbuffer::double_mapped_buffer::pagesize;
use vmcircbuffer::double_mapped_buffer::set_pool_limit;

fn addr<T>(b: &DoubleMappedBuffer<T>) -> usize {
    unsafe { b.slice().as_ptr() as usize }
}

/// Mappings of buffer files the process holds.
#[cfg(target_os = "linux")]
fn mappings() -> usize {
    std::fs::read_to_string("/proc/self/maps")
        .unwrap()
        .lines()
        .filter(|l| l.contains("buffer") && l.ends_with("(deleted)"))
        .count()
}

#[test]
fn dropped_mappings_are_reused_up_to_the_limit() {
    let bytes = 5 * pagesize();
    #[cfg(target_os = "linux")]
    let before = mappings();

    // Reused, and initialized again, here for another item type.
    let b = DoubleMappedBuffer::<u8>::new(bytes).unwrap();
    let first = addr(&b);
    unsafe { b.slice_mut().fill(0xff) };
    drop(b);
    let b = DoubleMappedBuffer::<u32>::new(bytes / 4).unwrap();
    assert_eq!(addr(&b), first, "the mapping is reused");
    unsafe {
        assert!(b.slice().iter().all(|v| *v == 0), "items are reset");
        b.slice_mut()[0] = 7;
        assert_eq!(b.slice_with_offset(b.capacity())[0], 7, "double mapped");
    }

    // In use: another buffer of that size gets its own mapping.
    let c = DoubleMappedBuffer::<u32>::new(bytes / 4).unwrap();
    assert_ne!(addr(&c), first);
    drop(b);
    drop(c);
    #[cfg(target_os = "linux")]
    assert!(mappings() > before, "dropped mappings are kept");

    // A lower limit releases what is kept beyond it.
    assert_eq!(set_pool_limit(0), DEFAULT_POOL_LIMIT);
    #[cfg(target_os = "linux")]
    assert_eq!(mappings(), before, "nothing is kept");
    drop(DoubleMappedBuffer::<u8>::new(bytes).unwrap());
    #[cfg(target_os = "linux")]
    assert_eq!(mappings(), before, "nothing is kept");
    assert_eq!(set_pool_limit(DEFAULT_POOL_LIMIT), 0);
}
