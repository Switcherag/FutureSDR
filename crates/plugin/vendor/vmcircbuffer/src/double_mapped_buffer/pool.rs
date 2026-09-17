//! Mappings of dropped buffers, kept for new buffers of the same size.
//!
//! Creating a double mapping takes several system calls, and each of its
//! pages faults on first use; releasing it makes the kernel flush the TLBs of
//! every core the process ran on. Applications that create and drop buffers
//! over and over (e.g., when restarting a processing graph) ask for the same
//! sizes again and again, so dropped mappings are kept for reuse, up to a
//! limit.

use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;

use super::DoubleMappedBufferError;
use super::Mapping;
use super::pagesize;

/// Initial limit of the pool, see [`set_pool_limit`].
pub const DEFAULT_POOL_LIMIT: usize = 64 << 20;

/// How often creating a mapping is tried before giving up.
const ATTEMPTS: usize = 6;

struct Pool {
    /// Oldest first.
    mappings: Vec<Mapping>,
    /// Memory held, counting both mappings of each buffer.
    bytes: usize,
    limit: usize,
}

static POOL: Mutex<Pool> = Mutex::new(Pool {
    mappings: Vec::new(),
    bytes: 0,
    limit: DEFAULT_POOL_LIMIT,
});

fn pool() -> MutexGuard<'static, Pool> {
    POOL.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Pool {
    /// A mapping of `size` bytes whose address is a multiple of `alignment`,
    /// the most recently dropped one first.
    fn take(&mut self, size: usize, alignment: usize) -> Option<Mapping> {
        let i = self
            .mappings
            .iter()
            .rposition(|m| m.size() == size && m.addr().is_multiple_of(alignment))?;
        let mapping = self.mappings.remove(i);
        self.bytes -= 2 * size;
        Some(mapping)
    }

    /// Keep `mapping`, releasing the oldest ones beyond the limit. Returns
    /// the mappings to release, to be dropped outside the lock.
    fn give(&mut self, mapping: Mapping) -> Vec<Mapping> {
        self.bytes += 2 * mapping.size();
        self.mappings.push(mapping);
        self.trim()
    }

    fn trim(&mut self) -> Vec<Mapping> {
        let mut excess = 0;
        let mut bytes = self.bytes;
        for m in &self.mappings {
            if bytes <= self.limit {
                break;
            }
            bytes -= 2 * m.size();
            excess += 1;
        }
        self.bytes = bytes;
        self.mappings.drain(..excess).collect()
    }
}

/// Set how much memory dropped buffers may keep for reuse, counting both
/// mappings of each buffer, and return the previous limit.
///
/// The limit starts at [`DEFAULT_POOL_LIMIT`]; `0` turns reuse off. Kept
/// mappings beyond the new limit are released.
pub fn set_pool_limit(bytes: usize) -> usize {
    let (previous, released) = {
        let mut pool = pool();
        let previous = std::mem::replace(&mut pool.limit, bytes);
        (previous, pool.trim())
    };
    drop(released);
    previous
}

#[derive(Debug)]
pub struct DoubleMappedBufferImpl {
    /// Always `Some`, except while being dropped.
    mapping: Option<Mapping>,
    item_size: usize,
}

impl DoubleMappedBufferImpl {
    pub fn new(
        min_items: usize,
        item_size: usize,
        alignment: usize,
    ) -> Result<Self, DoubleMappedBufferError> {
        let ps = pagesize();
        let mut size = ps;
        while size < min_items * item_size || !size.is_multiple_of(item_size) {
            size += ps;
        }

        let pooled = pool().take(size, alignment);
        let mapping = match pooled {
            Some(mapping) => mapping,
            None => {
                let mut attempt = 1;
                loop {
                    match Mapping::new(size, alignment) {
                        Ok(mapping) => break mapping,
                        Err(e) if attempt == ATTEMPTS => return Err(e),
                        Err(_) => attempt += 1,
                    }
                }
            }
        };
        Ok(Self {
            mapping: Some(mapping),
            item_size,
        })
    }

    fn mapping(&self) -> &Mapping {
        self.mapping.as_ref().expect("mapping is present")
    }

    pub fn addr(&self) -> usize {
        self.mapping().addr()
    }

    pub fn capacity(&self) -> usize {
        self.mapping().size() / self.item_size
    }
}

impl Drop for DoubleMappedBufferImpl {
    fn drop(&mut self) {
        if let Some(mapping) = self.mapping.take() {
            let released = pool().give(mapping);
            drop(released);
        }
    }
}
