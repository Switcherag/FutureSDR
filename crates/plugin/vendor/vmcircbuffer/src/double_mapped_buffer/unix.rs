use std::ffi::CString;
use std::os::unix::ffi::OsStringExt;

use super::DoubleMappedBufferError;

/// A new, empty file only this process can reach, open for reading and
/// writing.
fn shared_memory() -> Result<libc::c_int, DoubleMappedBufferError> {
    #[cfg(target_os = "linux")]
    {
        let fd = unsafe { libc::memfd_create(c"buffer".as_ptr(), libc::MFD_CLOEXEC) };
        if fd >= 0 {
            return Ok(fd);
        }
        // e.g., memfd_create denied by a seccomp filter: use a temporary file
    }

    let mut path = std::env::temp_dir();
    path.push("buffer-XXXXXX");
    let mut template = CString::new(path.into_os_string().into_vec())
        .map_err(|_| DoubleMappedBufferError::Create)?
        .into_bytes_with_nul();
    unsafe {
        let fd = libc::mkstemp(template.as_mut_ptr().cast::<libc::c_char>());
        if fd < 0 {
            return Err(DoubleMappedBufferError::Create);
        }
        if libc::unlink(template.as_ptr().cast::<libc::c_char>()) < 0 {
            libc::close(fd);
            return Err(DoubleMappedBufferError::Unlink);
        }
        Ok(fd)
    }
}

/// A file of `size` bytes, mapped twice, back-to-back.
#[derive(Debug)]
pub struct Mapping {
    addr: usize,
    size: usize,
}

impl Mapping {
    /// Map `size` bytes, a multiple of the page size, twice, at an address
    /// that is a multiple of `alignment`.
    pub fn new(size: usize, alignment: usize) -> Result<Self, DoubleMappedBufferError> {
        let fd = shared_memory()?;
        let buff;
        unsafe {
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
                libc::munmap(buff, 2 * size);
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
                libc::munmap(buff, 2 * size);
                libc::close(fd);
                return Err(DoubleMappedBufferError::MapSecond);
            }

            let ret = libc::ftruncate(fd, size as libc::off_t);
            if ret < 0 {
                libc::munmap(buff, 2 * size);
                libc::close(fd);
                return Err(DoubleMappedBufferError::Truncate);
            }

            let ret = libc::close(fd);
            if ret < 0 {
                libc::munmap(buff, 2 * size);
                return Err(DoubleMappedBufferError::Close);
            }
        }

        Ok(Mapping {
            addr: buff as usize,
            size,
        })
    }

    pub fn addr(&self) -> usize {
        self.addr
    }

    pub fn size(&self) -> usize {
        self.size
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.addr as *mut libc::c_void, self.size * 2);
        }
    }
}
