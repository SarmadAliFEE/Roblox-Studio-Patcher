use core::mem::MaybeUninit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemError {
    Unmapped,
    BadPointer,
}

pub type MemResult<T> = Result<T, MemError>;

const MIN_PLAUSIBLE_ADDR: usize = 0x1000;
const MAX_PLAUSIBLE_ADDR: usize = 0x0000_7fff_ffff_ffff;

#[inline]
pub fn looks_like_pointer(value: usize) -> bool {
    (MIN_PLAUSIBLE_ADDR..=MAX_PLAUSIBLE_ADDR).contains(&value)
}

pub fn read_bytes(addr: usize, out: &mut [u8]) -> MemResult<()> {
    if !looks_like_pointer(addr) || out.is_empty() {
        return Err(MemError::BadPointer);
    }
    imp::read_bytes(addr, out)
}

pub fn write_bytes(addr: usize, data: &[u8]) -> MemResult<()> {
    if !looks_like_pointer(addr) || data.is_empty() {
        return Err(MemError::BadPointer);
    }
    imp::write_bytes(addr, data)
}

pub fn read<T: Copy>(addr: usize) -> MemResult<T> {
    let mut value = MaybeUninit::<T>::uninit();
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(value.as_mut_ptr() as *mut u8, core::mem::size_of::<T>())
    };
    read_bytes(addr, bytes)?;
    Ok(unsafe { value.assume_init() })
}

pub fn write<T: Copy>(addr: usize, value: T) -> MemResult<()> {
    let bytes = unsafe {
        core::slice::from_raw_parts(&value as *const T as *const u8, core::mem::size_of::<T>())
    };
    write_bytes(addr, bytes)
}

pub fn read_ptr(addr: usize) -> MemResult<usize> {
    let value: usize = read(addr)?;
    if looks_like_pointer(value) {
        Ok(value)
    } else {
        Err(MemError::BadPointer)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{MemError, MemResult};
    use mach2::kern_return::KERN_SUCCESS;
    use mach2::traps::mach_task_self;
    use mach2::vm::{mach_vm_read_overwrite, mach_vm_write};
    use mach2::vm_types::mach_vm_address_t;

    pub fn read_bytes(addr: usize, out: &mut [u8]) -> MemResult<()> {
        let mut got: mach2::vm_types::mach_vm_size_t = 0;
        let kr = unsafe {
            mach_vm_read_overwrite(
                mach_task_self(),
                addr as mach_vm_address_t,
                out.len() as u64,
                out.as_mut_ptr() as mach_vm_address_t,
                &mut got,
            )
        };
        if kr == KERN_SUCCESS && got as usize == out.len() {
            Ok(())
        } else {
            Err(MemError::Unmapped)
        }
    }

    pub fn write_bytes(addr: usize, data: &[u8]) -> MemResult<()> {
        let kr = unsafe {
            mach_vm_write(
                mach_task_self(),
                addr as mach_vm_address_t,
                data.as_ptr() as mach2::vm_types::vm_offset_t,
                data.len() as u32,
            )
        };
        if kr == KERN_SUCCESS {
            Ok(())
        } else {
            Err(MemError::Unmapped)
        }
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::{MemError, MemResult};
    use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub fn read_bytes(addr: usize, out: &mut [u8]) -> MemResult<()> {
        let mut got: usize = 0;
        let ok = unsafe {
            ReadProcessMemory(
                GetCurrentProcess(),
                addr as *const core::ffi::c_void,
                out.as_mut_ptr() as *mut core::ffi::c_void,
                out.len(),
                &mut got,
            )
        };
        if ok != 0 && got == out.len() {
            Ok(())
        } else {
            Err(MemError::Unmapped)
        }
    }

    pub fn write_bytes(addr: usize, data: &[u8]) -> MemResult<()> {
        let mut wrote: usize = 0;
        let ok = unsafe {
            WriteProcessMemory(
                GetCurrentProcess(),
                addr as *mut core::ffi::c_void,
                data.as_ptr() as *const core::ffi::c_void,
                data.len(),
                &mut wrote,
            )
        };
        if ok != 0 && wrote == data.len() {
            Ok(())
        } else {
            Err(MemError::Unmapped)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_back_a_local() {
        let value: u64 = 0x0123_4567_89ab_cdef;
        let got: u64 = read(&value as *const u64 as usize).expect("own stack is readable");
        assert_eq!(got, value);
    }

    #[test]
    fn rejects_null_and_low_addresses() {
        assert_eq!(read::<u64>(0), Err(MemError::BadPointer));
        assert_eq!(read::<u64>(0x10), Err(MemError::BadPointer));
    }

    #[test]
    fn unmapped_address_is_an_error_not_a_crash() {
        assert_eq!(read::<u64>(0x0000_7ffe_dead_0000), Err(MemError::Unmapped));
    }

    #[test]
    fn writes_then_reads_back() {
        let mut cell: u64 = 0;
        let addr = &mut cell as *mut u64 as usize;
        write(addr, 0xfeed_face_u64).expect("own stack is writable");
        assert_eq!(cell, 0xfeed_face);
    }
}
