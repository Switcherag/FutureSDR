use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Mutex;

use super::DoubleMappedBufferError;
use super::pagesize;

/// Double mappings of dropped buffers, kept for reuse.
///
/// Mapping a buffer takes a temporary file, six system calls and a page
/// fault per page on first use; unmapping it interrupts every core the
/// process ran on. Flowgraphs that are started and stopped over and over
/// allocate the same sizes again and again, so the mappings are kept.
static POOL: Mutex<Pool> = Mutex::new(Pool {
    buffers: Vec::new(),
    bytes: 0,
});

/// Most memory the pool keeps, counting both mappings of each buffer.
const POOL_MAX_BYTES: usize = 64 << 20;

struct Pool {
    /// (address, size of one mapping)
    buffers: Vec<(usize, usize)>,
    bytes: usize,
}

impl Pool {
    fn take(&mut self, size: usize, alignment: usize) -> Option<usize> {
        let i = self
            .buffers
            .iter()
            .rposition(|(addr, s)| *s == size && addr.is_multiple_of(alignment))?;
        let (addr, _) = self.buffers.swap_remove(i);
        self.bytes -= 2 * size;
        Some(addr)
    }

    fn give(&mut self, addr: usize, size: usize) -> bool {
        if self.bytes + 2 * size > POOL_MAX_BYTES {
            return false;
        }
        self.buffers.push((addr, size));
        self.bytes += 2 * size;
        true
    }
}

#[derive(Debug)]
pub struct DoubleMappedBufferImpl {
    addr: usize,
    size_bytes: usize,
    item_size: usize,
}

impl DoubleMappedBufferImpl {
    pub fn new(
        min_items: usize,
        item_size: usize,
        alignment: usize,
    ) -> Result<Self, DoubleMappedBufferError> {
        for _ in 0..5 {
            let ret = Self::new_try(min_items, item_size, alignment);
            if ret.is_ok() {
                return ret;
            }
        }
        Self::new_try(min_items, item_size, alignment)
    }

    fn new_try(
        min_items: usize,
        item_size: usize,
        alignment: usize,
    ) -> Result<Self, DoubleMappedBufferError> {
        let ps = pagesize();
        let mut size = ps;
        while size < min_items * item_size || !size.is_multiple_of(item_size) {
            size += ps;
        }

        let pooled = POOL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take(size, alignment);
        if let Some(addr) = pooled {
            return Ok(DoubleMappedBufferImpl {
                addr,
                size_bytes: size,
                item_size,
            });
        }

        let tmp = std::env::temp_dir();
        let mut path = PathBuf::new();
        path.push(tmp);
        path.push("buffer-XXXXXX");
        let cstring = CString::new(path.into_os_string().as_bytes()).unwrap();
        let path = cstring.as_bytes_with_nul().as_ptr();

        let fd;
        let buff;
        unsafe {
            fd = libc::mkstemp(path as *mut libc::c_char);
            if fd < 0 {
                return Err(DoubleMappedBufferError::Create);
            }

            let ret = libc::unlink(path.cast::<libc::c_char>());
            if ret < 0 {
                libc::close(fd);
                return Err(DoubleMappedBufferError::Unlink);
            }

            let ret = libc::ftruncate(fd, 2 * size as libc::off_t);
            if ret < 0 {
                libc::close(fd);
                return Err(DoubleMappedBufferError::Truncate);
            }

            buff = libc::mmap(
                std::ptr::null_mut::<libc::c_void>(),
                2 * size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            );
            if buff == libc::MAP_FAILED {
                libc::close(fd);
                return Err(DoubleMappedBufferError::Placeholder);
            }
            if !(buff as usize).is_multiple_of(alignment) {
                libc::close(fd);
                return Err(DoubleMappedBufferError::Alignment);
            }

            let buff2 = libc::mmap(
                buff.add(size),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_FIXED,
                fd,
                0,
            );
            if buff2 != buff.add(size) {
                libc::munmap(buff, size);
                libc::close(fd);
                return Err(DoubleMappedBufferError::MapSecond);
            }

            let ret = libc::ftruncate(fd, size as libc::off_t);
            if ret < 0 {
                libc::munmap(buff, size);
                libc::munmap(buff2, size);
                libc::close(fd);
                return Err(DoubleMappedBufferError::Truncate);
            }

            let ret = libc::close(fd);
            if ret < 0 {
                return Err(DoubleMappedBufferError::Close);
            }
        }

        Ok(DoubleMappedBufferImpl {
            addr: buff as usize,
            size_bytes: size,
            item_size,
        })
    }

    pub fn addr(&self) -> usize {
        self.addr
    }

    pub fn capacity(&self) -> usize {
        self.size_bytes / self.item_size
    }
}

impl Drop for DoubleMappedBufferImpl {
    fn drop(&mut self) {
        let kept = POOL
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .give(self.addr, self.size_bytes);
        if kept {
            return;
        }
        unsafe {
            libc::munmap(self.addr as *mut libc::c_void, self.size_bytes * 2);
        }
    }
}
