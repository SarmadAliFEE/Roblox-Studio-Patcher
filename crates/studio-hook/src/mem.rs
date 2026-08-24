//! Fallible reads and writes against another component's live memory.
//!
//! Every structural search in this crate works by guessing an address and
//! seeing whether what lands there looks right, so the overwhelming
//! majority of accesses are against addresses that are not mapped at all.
//! That makes the read primitive the single most performance- and
//! correctness-critical piece here.
//!
//! The C++ predecessor did this with a process-wide SIGSEGV/SIGBUS handler
//! and `siglongjmp` out of the faulting access. That had three problems
//! which are all structural rather than incidental: a fault costs a full
//! signal round trip (milliseconds at the volume these scans generate), the
//! handler is global so it intercepts faults belonging to the host process
//! too, and `longjmp` cannot cross Rust frames without skipping destructors.
//!
//! Both target platforms expose a kernel call that reports an unreadable
//! address by returning an error instead of raising a fault, so none of
//! that machinery is needed: `mach_vm_read_overwrite` on macOS and
//! `ReadProcessMemory` on Windows. Failure here is an ordinary `Err`.

use core::mem::MaybeUninit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemError {
    /// The address range is not readable/writable in this process.
    Unmapped,
    /// A null or obviously-bogus pointer; never handed to the kernel.
    BadPointer,
}

pub type MemResult<T> = Result<T, MemError>;

/// Userspace pointers on both supported targets live well below this.
/// Cheap arithmetic rejection before paying for a syscall.
const MIN_PLAUSIBLE_ADDR: usize = 0x1000;
const MAX_PLAUSIBLE_ADDR: usize = 0x0000_7fff_ffff_ffff;

#[inline]
pub fn looks_like_pointer(value: usize) -> bool {
    (MIN_PLAUSIBLE_ADDR..=MAX_PLAUSIBLE_ADDR).contains(&value)
}

/// Reads `len` bytes at `addr` into `out`, reporting unreadable memory as
/// an error rather than faulting.
pub fn read_bytes(addr: usize, out: &mut [u8]) -> MemResult<()> {
    if !looks_like_pointer(addr) || out.is_empty() {
        return Err(MemError::BadPointer);
    }
    imp::read_bytes(addr, out)
}

/// Writes `data` at `addr`, reporting unwritable memory as an error rather
/// than faulting.
pub fn write_bytes(addr: usize, data: &[u8]) -> MemResult<()> {
    if !looks_like_pointer(addr) || data.is_empty() {
        return Err(MemError::BadPointer);
    }
    imp::write_bytes(addr, data)
}

/// Reads a `Copy` value out of the target's memory.
///
/// `T` must be plain old data with no padding invariants and no pointer
/// validity requirements, since the bytes come from memory this process
/// does not own the layout of. Every current caller uses integers and raw
/// addresses, which satisfy that.
pub fn read<T: Copy>(addr: usize) -> MemResult<T> {
    let mut value = MaybeUninit::<T>::uninit();
    // SAFETY: the slice covers exactly the bytes of `value`, and it is only
    // assumed initialised on the success path below.
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(value.as_mut_ptr() as *mut u8, core::mem::size_of::<T>())
    };
    read_bytes(addr, bytes)?;
    // SAFETY: read_bytes returning Ok means every byte was written.
    Ok(unsafe { value.assume_init() })
}

pub fn write<T: Copy>(addr: usize, value: T) -> MemResult<()> {
    // SAFETY: reading the bytes of a live, initialised local.
    let bytes = unsafe {
        core::slice::from_raw_parts(&value as *const T as *const u8, core::mem::size_of::<T>())
    };
    write_bytes(addr, bytes)
}

/// Reads a pointer-sized field and rejects values that cannot be pointers,
/// which is the check nearly every structural probe wants.
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
        // SAFETY: reading our own task; `out` is a valid writable slice of
        // exactly the requested length. An unreadable source address is
        // reported through the return code, not a fault.
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
        // SAFETY: writing into our own task from a valid slice; an
        // unwritable destination comes back as a non-success return code.
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
        // SAFETY: reading our own process; `out` is valid for `out.len()`
        // bytes. An unreadable source is reported by the FALSE return,
        // which is exactly why this is used instead of a raw deref.
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
        // SAFETY: writing into our own process from a valid slice; an
        // unwritable destination is reported by the FALSE return.
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
        // The whole point of the layer: this must return, not fault.
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
