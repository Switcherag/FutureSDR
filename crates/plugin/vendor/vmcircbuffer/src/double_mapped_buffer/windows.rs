use winapi::shared::minwindef::DWORD;
use winapi::shared::minwindef::LPCVOID;
use winapi::shared::minwindef::LPVOID;
use winapi::um::handleapi::CloseHandle;
use winapi::um::handleapi::INVALID_HANDLE_VALUE;
use winapi::um::memoryapi::MapViewOfFileEx;
use winapi::um::memoryapi::VirtualAlloc;
use winapi::um::memoryapi::VirtualFree;
use winapi::um::winnt::HANDLE;
use winapi::um::winnt::MEM_RELEASE;
use winapi::um::winnt::MEM_RESERVE;
use winapi::um::winnt::PAGE_NOACCESS;
use winapi::um::winnt::PAGE_READWRITE;
use winapi::um::{
    memoryapi::{FILE_MAP_WRITE, UnmapViewOfFile},
    winbase::CreateFileMappingA,
};

use super::DoubleMappedBufferError;

/// A file mapping of `size` bytes, mapped twice, back-to-back.
#[derive(Debug)]
pub struct Mapping {
    addr: usize,
    handle: usize,
    size: usize,
}

impl Mapping {
    /// Map `size` bytes, a multiple of the allocation granularity, twice, at
    /// an address that is a multiple of `alignment`.
    pub fn new(size: usize, alignment: usize) -> Result<Self, DoubleMappedBufferError> {
        unsafe {
            let handle = CreateFileMappingA(
                INVALID_HANDLE_VALUE,
                std::mem::zeroed(),
                PAGE_READWRITE,
                0,
                size as DWORD,
                std::ptr::null(),
            );

            if handle == INVALID_HANDLE_VALUE || handle == 0 as LPVOID {
                return Err(DoubleMappedBufferError::Placeholder);
            }

            let first_tmp =
                VirtualAlloc(std::ptr::null_mut(), 2 * size, MEM_RESERVE, PAGE_NOACCESS);
            if first_tmp.is_null() {
                CloseHandle(handle);
                return Err(DoubleMappedBufferError::MapFirst);
            }

            let res = VirtualFree(first_tmp, 0, MEM_RELEASE);
            if res == 0 {
                CloseHandle(handle);
                return Err(DoubleMappedBufferError::MapSecond);
            }

            let first_cpy = MapViewOfFileEx(handle, FILE_MAP_WRITE, 0, 0, size, first_tmp);
            if first_tmp != first_cpy {
                CloseHandle(handle);
                return Err(DoubleMappedBufferError::MapFirst);
            }

            if !(first_tmp as usize).is_multiple_of(alignment) {
                UnmapViewOfFile(first_cpy);
                CloseHandle(handle);
                return Err(DoubleMappedBufferError::Alignment);
            }

            let first_ptr = (first_tmp as *mut u8).add(size) as LPVOID;
            let second_cpy = MapViewOfFileEx(handle, FILE_MAP_WRITE, 0, 0, size, first_ptr);
            if second_cpy != first_ptr {
                UnmapViewOfFile(first_cpy);
                CloseHandle(handle);
                return Err(DoubleMappedBufferError::MapSecond);
            }

            Ok(Mapping {
                addr: first_tmp as usize,
                handle: handle as usize,
                size,
            })
        }
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
            UnmapViewOfFile(self.addr as LPCVOID);
            UnmapViewOfFile((self.addr + self.size) as LPCVOID);
            CloseHandle(self.handle as HANDLE);
        }
    }
}
