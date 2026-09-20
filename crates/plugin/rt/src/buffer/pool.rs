//! Buffers kept for the next flowgraph.
//!
//! Creating a double mapping costs a temporary file, several system calls and
//! a page fault per page on first use; releasing one makes the kernel flush
//! the TLB of every core the process ran on. A host that replaces flowgraphs
//! asks for the same buffers over and over, so buffers whose flowgraph is
//! gone are kept here, by item type and capacity, and handed to the next one
//! that fits.

use std::any::Any;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::OnceLock;

/// Bytes of buffer kept by default.
pub const DEFAULT_POOL_LIMIT: usize = 64 << 20;

struct Kept {
    buffer: Box<dyn Any + Send>,
    bytes: usize,
}

#[derive(Default)]
struct Pool {
    limit: usize,
    bytes: usize,
    /// By buffer type and capacity in items.
    kept: HashMap<(TypeId, usize), Vec<Kept>>,
}

fn pool() -> MutexGuard<'static, Pool> {
    static POOL: OnceLock<Mutex<Pool>> = OnceLock::new();
    let pool = POOL.get_or_init(|| {
        Mutex::new(Pool {
            limit: DEFAULT_POOL_LIMIT,
            ..Pool::default()
        })
    });
    pool.lock().unwrap_or_else(|e| e.into_inner())
}

/// A buffer of `capacity` items, if one was kept.
pub(crate) fn take<B: Any + Send>(capacity: usize) -> Option<B> {
    let mut pool = pool();
    let kept = pool.kept.get_mut(&(TypeId::of::<B>(), capacity))?.pop()?;
    pool.bytes -= kept.bytes;
    // The key holds the type, so this is the type that was kept.
    kept.buffer.downcast::<B>().ok().map(|b| *b)
}

/// Keep `buffer`, of `capacity` items and `bytes` bytes, for the next
/// flowgraph that asks for one like it.
pub(crate) fn keep<B: Any + Send>(capacity: usize, bytes: usize, buffer: B) {
    let mut pool = pool();
    if pool.bytes + bytes > pool.limit {
        return;
    }
    pool.bytes += bytes;
    pool.kept
        .entry((TypeId::of::<B>(), capacity))
        .or_default()
        .push(Kept {
            buffer: Box::new(buffer),
            bytes,
        });
}

/// Keep at most `bytes` of buffer; returns what the limit was. Zero drops
/// what is kept and stops keeping, which is what a program that builds its
/// flowgraph once wants.
pub fn set_pool_limit(bytes: usize) -> usize {
    let mut pool = pool();
    let was = pool.limit;
    pool.limit = bytes;
    while pool.bytes > pool.limit {
        let Some(key) = pool.kept.keys().next().copied() else {
            break;
        };
        let Some(kept) = pool.kept.get_mut(&key).and_then(Vec::pop) else {
            pool.kept.remove(&key);
            continue;
        };
        pool.bytes -= kept.bytes;
    }
    was
}

/// Bytes and buffers kept.
pub fn pool_stats() -> (usize, usize) {
    let pool = pool();
    (pool.bytes, pool.kept.values().map(Vec::len).sum())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pool is process-wide, so its tests take turns.
    fn alone() -> MutexGuard<'static, ()> {
        static ALONE: Mutex<()> = Mutex::new(());
        let guard = ALONE.lock().unwrap_or_else(|e| e.into_inner());
        set_pool_limit(0);
        set_pool_limit(DEFAULT_POOL_LIMIT);
        guard
    }

    #[test]
    fn a_kept_buffer_comes_back_once() {
        let _alone = alone();
        assert_eq!(take::<Vec<u8>>(8), None, "nothing is kept yet");
        keep(8, 8, vec![1u8, 2]);
        assert_eq!(pool_stats(), (8, 1));
        assert_eq!(take::<Vec<u8>>(8), Some(vec![1, 2]));
        assert_eq!(pool_stats(), (0, 0));
        assert_eq!(take::<Vec<u8>>(8), None, "and only once");
    }

    #[test]
    fn buffers_are_kept_by_type_and_capacity() {
        let _alone = alone();
        keep(8, 8, vec![1u8]);
        keep(16, 16, vec![2u8]);
        keep(8, 32, vec![3u32]);
        assert_eq!(take::<Vec<u8>>(32), None, "no buffer of that capacity");
        assert_eq!(take::<Vec<u16>>(8), None, "no buffer of that type");
        assert_eq!(take::<Vec<u32>>(8), Some(vec![3]));
        assert_eq!(take::<Vec<u8>>(8), Some(vec![1]));
        assert_eq!(take::<Vec<u8>>(16), Some(vec![2]));
        assert_eq!(pool_stats(), (0, 0));
    }

    #[test]
    fn the_limit_bounds_what_is_kept() {
        let _alone = alone();
        assert_eq!(set_pool_limit(24), DEFAULT_POOL_LIMIT, "what it was");
        keep(8, 16, vec![1u8]);
        keep(8, 16, vec![2u8]);
        assert_eq!(pool_stats(), (16, 1), "the second one does not fit");
        keep(8, 8, vec![3u8]);
        assert_eq!(pool_stats(), (24, 2), "one that fits still is");
    }

    #[test]
    fn a_limit_of_zero_empties_the_pool() {
        let _alone = alone();
        keep(8, 16, vec![1u8]);
        keep(16, 16, vec![2u8]);
        assert_eq!(pool_stats(), (32, 2));
        set_pool_limit(0);
        assert_eq!(pool_stats(), (0, 0));
        keep(8, 16, vec![3u8]);
        assert_eq!(pool_stats(), (0, 0), "and stops keeping");
    }
}
