use std::time::Duration;

#[cfg(target_os = "macos")]
const CONFIG_PATH: &str = "/Users/Shared/rbx-theme-set/Logger.json";
#[cfg(target_os = "windows")]
const CONFIG_PATH: &str = r"C:\Users\Public\rbxthemeset\Logger.json";

/// Installs local crash logging when `Logger.json` enables it.
///
/// Panics and fatal signals are appended to [`crate::crash_log_path`]; the
/// handlers chain down to Roblox's own so its crash reporting still runs.
pub fn init() {
    if !enabled() {
        return;
    }
    install_panic_hook();
    crate::log(&format!("logger online - pid {} - {}", std::process::id(), os_line()));
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(6));
        crash::install();
    });
}

fn enabled() -> bool {
    let Ok(text) = std::fs::read_to_string(CONFIG_PATH) else { return false };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { return false };
    json.get("enabled").and_then(serde_json::Value::as_bool).unwrap_or(false)
}

fn os_line() -> String {
    let arch = std::env::consts::ARCH;
    #[cfg(target_os = "macos")]
    {
        let mut os = std::env::consts::OS.to_string();
        if let Ok(out) = std::process::Command::new("sw_vers").arg("-productVersion").output() {
            if out.status.success() {
                os.push(' ');
                os.push_str(String::from_utf8_lossy(&out.stdout).trim());
            }
        }
        return format!("{os} ({arch})");
    }
    #[cfg(target_os = "windows")]
    {
        let mut os = std::env::consts::OS.to_string();
        if let Some(version) = windows_version() {
            os.push(' ');
            os.push_str(&version);
        }
        return format!("{os} ({arch})");
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    format!("{} ({arch})", std::env::consts::OS)
}

#[cfg(target_os = "windows")]
fn windows_version() -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("cmd")
        .args(["/c", "ver"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let line = raw.lines().map(str::trim).find(|line| !line.is_empty())?;
    Some(line.to_owned())
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        crate::log(&format!("panic: {info}"));
        previous(info);
    }));
}

#[cfg(unix)]
mod crash {
    use core::ffi::c_void;
    use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

    static WRITE_FD: AtomicI32 = AtomicI32::new(-1);
    static IMAGE_BASE: AtomicUsize = AtomicUsize::new(0);
    static HOOK_BASE: AtomicUsize = AtomicUsize::new(0);
    static PREV_HANDLER: [AtomicUsize; 6] = [const { AtomicUsize::new(0) }; 6];
    static PREV_FLAGS: [AtomicI32; 6] = [const { AtomicI32::new(0) }; 6];

    const SIGNALS: [i32; 6] =
        [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE, libc::SIGABRT, libc::SIGTRAP];

    unsafe extern "C" {
        fn backtrace(buffer: *mut *mut c_void, size: i32) -> i32;
    }

    pub fn install() {
        let Some(write_fd) = open_crash_log() else { return };
        WRITE_FD.store(write_fd, Ordering::Release);
        // Main-executable base so backtrace addresses resolve to offsets.
        if let Some(image) = crate::platform::find_main_image() {
            IMAGE_BASE.store(image.base(), Ordering::Release);
        }
        let mut dl_info: libc::Dl_info = unsafe { core::mem::zeroed() };
        if unsafe { libc::dladdr(handler as *const c_void, &mut dl_info) } != 0 {
            HOOK_BASE.store(dl_info.dli_fbase as usize, Ordering::Release);
        }

        unsafe {
            let stack_size = libc::SIGSTKSZ.max(64 * 1024);
            let mem = libc::malloc(stack_size);
            if !mem.is_null() {
                let stack = libc::stack_t { ss_sp: mem, ss_flags: 0, ss_size: stack_size };
                libc::sigaltstack(&stack, core::ptr::null_mut());
            }
            for (index, &signal) in SIGNALS.iter().enumerate() {
                let mut action: libc::sigaction = core::mem::zeroed();
                action.sa_sigaction = handler as *const () as usize;
                action.sa_flags = libc::SA_ONSTACK | libc::SA_SIGINFO | libc::SA_NODEFER;
                libc::sigemptyset(&mut action.sa_mask);
                let mut old: libc::sigaction = core::mem::zeroed();
                libc::sigaction(signal, &action, &mut old);
                // Remember whatever Roblox already had so we can chain to it.
                PREV_HANDLER[index].store(old.sa_sigaction, Ordering::Release);
                PREV_FLAGS[index].store(old.sa_flags, Ordering::Release);
            }
        }

        crate::log(&format!(
            "crash handler armed - image=0x{:x} hook=0x{:x} -> {}",
            IMAGE_BASE.load(Ordering::Acquire),
            HOOK_BASE.load(Ordering::Acquire),
            crate::crash_log_path().display()
        ));
    }

    fn open_crash_log() -> Option<i32> {
        use std::os::unix::ffi::OsStrExt;
        let path = crate::crash_log_path();
        let mut bytes = path.as_os_str().as_bytes().to_vec();
        bytes.push(0);
        let fd = unsafe {
            libc::open(
                bytes.as_ptr() as *const libc::c_char,
                libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                0o644,
            )
        };
        (fd >= 0).then_some(fd)
    }

    extern "C" fn handler(signal: i32, info: *mut libc::siginfo_t, ctx: *mut c_void) {
        let fault = unsafe { info.as_ref() }.map(|i| unsafe { i.si_addr() } as usize).unwrap_or(0);

        // heap use - safe even with a corrupt heap).
        let mut buf = [0u8; 768];
        let mut len = 0;
        len = append(&mut buf, len, b"CRASH signal=");
        len += write_uint(&mut buf[len..], signal as u64, 10);
        len = append(&mut buf, len, b" addr=0x");
        len += write_uint(&mut buf[len..], fault as u64, 16);
        len = append(&mut buf, len, b" base=0x");
        len += write_uint(&mut buf[len..], IMAGE_BASE.load(Ordering::Acquire) as u64, 16);
        len = append(&mut buf, len, b" hook=0x");
        len += write_uint(&mut buf[len..], HOOK_BASE.load(Ordering::Acquire) as u64, 16);

        len = append(&mut buf, len, b" stack=");
        let mut frames: [*mut c_void; 32] = [core::ptr::null_mut(); 32];
        let count = unsafe { backtrace(frames.as_mut_ptr(), frames.len() as i32) };
        for i in 0..count.max(0) as usize {
            if i > 0 {
                len = append(&mut buf, len, b",");
            }
            len = append(&mut buf, len, b"0x");
            len += write_uint(&mut buf[len..], frames[i] as u64, 16);
        }
        len = append(&mut buf, len, b"\n");

        let fd = WRITE_FD.load(Ordering::Acquire);
        if fd >= 0 {
            unsafe { libc::write(fd, buf.as_ptr() as *const c_void, len) };
            // Hold the crashing thread briefly so the helper can curl the report
            // out before the process dies.
            let delay = libc::timespec { tv_sec: 3, tv_nsec: 0 };
            unsafe { libc::nanosleep(&delay, core::ptr::null_mut()) };
        }

        // Chain to Roblox's own handler (if any) so its crash reporting still runs.
        if let Some(index) = SIGNALS.iter().position(|&s| s == signal) {
            let previous = PREV_HANDLER[index].load(Ordering::Acquire);
            let flags = PREV_FLAGS[index].load(Ordering::Acquire);
            if previous > 1 {
                unsafe {
                    if flags & libc::SA_SIGINFO != 0 {
                        let previous: extern "C" fn(i32, *mut libc::siginfo_t, *mut c_void) =
                            core::mem::transmute(previous);
                        previous(signal, info, ctx);
                    } else {
                        let previous: extern "C" fn(i32) = core::mem::transmute(previous);
                        previous(signal);
                    }
                }
            }
        }

        unsafe {
            let mut action: libc::sigaction = core::mem::zeroed();
            action.sa_sigaction = libc::SIG_DFL;
            libc::sigaction(signal, &action, core::ptr::null_mut());
            libc::raise(signal);
        }
    }

    fn append(buf: &mut [u8], mut len: usize, bytes: &[u8]) -> usize {
        for &b in bytes {
            if len < buf.len() {
                buf[len] = b;
                len += 1;
            }
        }
        len
    }

    fn write_uint(out: &mut [u8], mut value: u64, radix: u64) -> usize {
        const DIGITS: &[u8] = b"0123456789abcdef";
        let mut tmp = [0u8; 20];
        let mut count = 0;
        loop {
            tmp[count] = DIGITS[(value % radix) as usize];
            count += 1;
            value /= radix;
            if value == 0 {
                break;
            }
        }
        for i in 0..count {
            if i < out.len() {
                out[i] = tmp[count - 1 - i];
            }
        }
        count.min(out.len())
    }
}

#[cfg(windows)]
mod crash {
    use core::ffi::c_void;
    use std::os::windows::io::IntoRawHandle;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS, RtlCaptureStackBackTrace,
        SetUnhandledExceptionFilter,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::Sleep;


    const ACCESS_VIOLATION: u32 = 0xC000_0005;

    static WRITE_HANDLE: AtomicUsize = AtomicUsize::new(0);
    static PREV_FILTER: AtomicUsize = AtomicUsize::new(0);
    static IMAGE_BASE: AtomicUsize = AtomicUsize::new(0);
    static HOOK_BASE: AtomicUsize = AtomicUsize::new(0);

    type FilterFn = unsafe extern "system" fn(*const EXCEPTION_POINTERS) -> i32;

    pub fn install() {
        if !open_crash_log() {
            return;
        }
        IMAGE_BASE.store(unsafe { GetModuleHandleW(core::ptr::null()) } as usize, Ordering::Release);
        let name: Vec<u16> = "studio_hook.dll".encode_utf16().chain(core::iter::once(0)).collect();
        let hook = unsafe { GetModuleHandleW(name.as_ptr()) } as usize;
        if hook != 0 {
            HOOK_BASE.store(hook, Ordering::Release);
        }
        // Chain to whatever was already registered (Roblox's own crash handler)
        // so their reporting still runs after ours.
        let previous = unsafe { SetUnhandledExceptionFilter(Some(handler)) };
        PREV_FILTER.store(previous.map(|f| f as usize).unwrap_or(0), Ordering::Release);

        crate::log(&format!(
            "crash handler armed - image=0x{:x} hook=0x{hook:x} -> {}",
            IMAGE_BASE.load(Ordering::Acquire),
            crate::crash_log_path().display()
        ));
    }

    fn open_crash_log() -> bool {
        let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(crate::crash_log_path())
        else {
            return false;
        };
        WRITE_HANDLE.store(file.into_raw_handle() as usize, Ordering::Release);
        true
    }

    unsafe extern "system" fn handler(info: *const EXCEPTION_POINTERS) -> i32 {
        let record = unsafe { info.as_ref().and_then(|p| p.ExceptionRecord.as_ref()) };
        let code = record.map(|r| r.ExceptionCode as u32).unwrap_or(0);
        let address = record.map(|r| r.ExceptionAddress as usize).unwrap_or(0);

        let mut buf = [0u8; 768];
        let mut len = 0;
        len = append(&mut buf, len, b"CRASH exception=0x");
        len += write_uint(&mut buf[len..], code as u64, 16);
        len = append(&mut buf, len, b" addr=0x");
        len += write_uint(&mut buf[len..], address as u64, 16);
        len = append(&mut buf, len, b" base=0x");
        len += write_uint(&mut buf[len..], IMAGE_BASE.load(Ordering::Acquire) as u64, 16);
        len = append(&mut buf, len, b" hook=0x");
        len += write_uint(&mut buf[len..], HOOK_BASE.load(Ordering::Acquire) as u64, 16);

        if code == ACCESS_VIOLATION {
            if let Some(record) = record {
                if record.NumberParameters >= 2 {
                    len = append(&mut buf, len, b" access=");
                    len = append(&mut buf, len, match record.ExceptionInformation[0] {
                        0 => b"read".as_slice(),
                        1 => b"write".as_slice(),
                        8 => b"execute".as_slice(),
                        _ => b"?".as_slice(),
                    });
                    len = append(&mut buf, len, b" fault=0x");
                    len += write_uint(&mut buf[len..], record.ExceptionInformation[1] as u64, 16);
                }
            }
        }

        len = append(&mut buf, len, b" stack=");
        let mut frames: [*mut c_void; 32] = [core::ptr::null_mut(); 32];
        let count = unsafe { RtlCaptureStackBackTrace(0, frames.len() as u32, frames.as_mut_ptr(), core::ptr::null_mut()) };
        for i in 0..count as usize {
            if i > 0 {
                len = append(&mut buf, len, b",");
            }
            len = append(&mut buf, len, b"0x");
            len += write_uint(&mut buf[len..], frames[i] as u64, 16);
        }
        len = append(&mut buf, len, b"\n");

        let handle = WRITE_HANDLE.load(Ordering::Acquire);
        if handle != 0 {
            let mut written = 0u32;
            unsafe {
                WriteFile(handle as *mut c_void, buf.as_ptr(), len as u32, &mut written, core::ptr::null_mut());
                Sleep(3000);
            }
        }

        let previous = PREV_FILTER.load(Ordering::Acquire);
        if previous != 0 {
            let previous: FilterFn = unsafe { core::mem::transmute(previous) };
            return unsafe { previous(info) };
        }
        EXCEPTION_CONTINUE_SEARCH
    }

    fn append(buf: &mut [u8], mut len: usize, bytes: &[u8]) -> usize {
        for &b in bytes {
            if len < buf.len() {
                buf[len] = b;
                len += 1;
            }
        }
        len
    }

    fn write_uint(out: &mut [u8], mut value: u64, radix: u64) -> usize {
        const DIGITS: &[u8] = b"0123456789abcdef";
        let mut tmp = [0u8; 20];
        let mut count = 0;
        loop {
            tmp[count] = DIGITS[(value % radix) as usize];
            count += 1;
            value /= radix;
            if value == 0 {
                break;
            }
        }
        for i in 0..count {
            if i < out.len() {
                out[i] = tmp[count - 1 - i];
            }
        }
        count.min(out.len())
    }
}
