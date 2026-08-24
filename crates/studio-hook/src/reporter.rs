use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
const CONFIG_PATH: &str = "/Users/Shared/rbx-theme-set/Logger.json";
#[cfg(target_os = "windows")]
const CONFIG_PATH: &str = r"C:\Users\Public\rbxthemeset\Logger.json";

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const MIN_POST_INTERVAL: Duration = Duration::from_millis(2000);
const MAX_LOG: usize = 512 * 1024;
const BOUNDARY: &str = "----studiohooklog7f3a2b";

static SENDER: OnceLock<Sender<()>> = OnceLock::new();
static LOG: Mutex<String> = Mutex::new(String::new());
static MESSAGE_ID: Mutex<Option<String>> = Mutex::new(None);

pub fn init() {
    let Some((enabled, webhook)) = load_config() else { return };
    if !enabled || webhook.is_empty() {
        return;
    }

    let (tx, rx) = mpsc::channel::<()>();
    if SENDER.set(tx).is_err() {
        return;
    }
    let hook = webhook.clone();
    std::thread::spawn(move || pump(rx, hook));

    install_panic_hook();
    // Install native crash handlers only after Studio has spun up its own, so ours
    // sit on top and chain down to Roblox's crash handler rather than being replaced.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(6));
        crash::install(webhook);
    });
    report(&format!("logger online - pid {} - {}", std::process::id(), os_line()));
}

pub fn report(message: &str) {
    {
        let mut log = LOG.lock().unwrap_or_else(|p| p.into_inner());
        log.push_str(message);
        log.push('\n');
        if log.len() > MAX_LOG {
            let mut cut = log.len() - MAX_LOG;
            while cut < log.len() && !log.is_char_boundary(cut) {
                cut += 1;
            }
            log.drain(..cut);
        }
    }
    if let Some(tx) = SENDER.get() {
        let _ = tx.send(());
    }
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
    #[cfg(not(target_os = "macos"))]
    format!("{} ({arch})", std::env::consts::OS)
}

fn load_config() -> Option<(bool, String)> {
    let text = std::fs::read_to_string(CONFIG_PATH).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let enabled = json.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
    let webhook = json.get("webhook").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
    Some((enabled, webhook))
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        report(&format!("PANIC: {info}"));
        previous(info);
    }));
}

fn pump(rx: Receiver<()>, webhook: String) {
    let mut last_post = Instant::now() - MIN_POST_INTERVAL;
    while rx.recv().is_ok() {
        while rx.try_recv().is_ok() {}
        let wait = MIN_POST_INTERVAL.saturating_sub(last_post.elapsed());
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
        while rx.try_recv().is_ok() {}
        flush(&webhook);
        last_post = Instant::now();
    }
}

fn flush(webhook: &str) {
    let content = {
        let log = LOG.lock().unwrap_or_else(|p| p.into_inner());
        if log.is_empty() {
            return;
        }
        log.clone()
    };
    let existing = MESSAGE_ID.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let updated = match existing {
        Some(id) => edit_log(webhook, &id, &content).or_else(|| create_log(webhook, &content)),
        None => create_log(webhook, &content),
    };
    if let Some(id) = updated {
        *MESSAGE_ID.lock().unwrap_or_else(|p| p.into_inner()) = Some(id);
    }
}

fn create_log(webhook: &str, content: &str) -> Option<String> {
    let response = multipart(&format!("{webhook}?wait=true"), "POST", content, None)?;
    let json: serde_json::Value = serde_json::from_str(&response).ok()?;
    json.get("id").and_then(|v| v.as_str()).map(|s| s.to_owned())
}

fn edit_log(webhook: &str, id: &str, content: &str) -> Option<String> {
    multipart(&format!("{webhook}/messages/{id}"), "PATCH", content, Some("{\"attachments\":[]}"))?;
    Some(id.to_owned())
}

fn multipart(url: &str, method: &str, file: &str, payload_json: Option<&str>) -> Option<String> {
    let mut body: Vec<u8> = Vec::with_capacity(file.len() + 512);
    if let Some(payload) = payload_json {
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"payload_json\"\r\n\
                 Content-Type: application/json\r\n\r\n{payload}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"files[0]\"; \
             filename=\"studio-hook.log\"\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(file.as_bytes());
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

    ureq::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .request(method, url)
        .set("Content-Type", &format!("multipart/form-data; boundary={BOUNDARY}"))
        .send_bytes(&body)
        .ok()?
        .into_string()
        .ok()
}

#[cfg(unix)]
mod crash {
    use core::ffi::c_void;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

    static WRITE_FD: AtomicI32 = AtomicI32::new(-1);
    static IMAGE_BASE: AtomicUsize = AtomicUsize::new(0);
    static PREV_HANDLER: [AtomicUsize; 6] = [const { AtomicUsize::new(0) }; 6];
    static PREV_FLAGS: [AtomicI32; 6] = [const { AtomicI32::new(0) }; 6];

    const SIGNALS: [i32; 6] =
        [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE, libc::SIGABRT, libc::SIGTRAP];

    unsafe extern "C" {
        fn backtrace(buffer: *mut *mut c_void, size: i32) -> i32;
    }

    pub fn install(webhook: String) {
        let Some(write_fd) = spawn_helper(&webhook) else { return };
        WRITE_FD.store(write_fd, Ordering::Release);
        // Main-executable base so backtrace addresses resolve to offsets.
        if let Some(image) = crate::platform::find_main_image() {
            IMAGE_BASE.store(image.base(), Ordering::Release);
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
    }

    fn spawn_helper(webhook: &str) -> Option<i32> {
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return None;
        }
        let (read_fd, write_fd) = (fds[0], fds[1]);
        let script = format!(
            "while IFS= read -r line; do curl -s -m 10 -H 'Content-Type: application/json' \
             --data-raw \"{{\\\"content\\\":\\\"$line\\\"}}\" '{webhook}' >/dev/null 2>&1; done"
        );
        let stdin = unsafe { OwnedFd::from_raw_fd(read_fd) };
        let spawned = Command::new("sh")
            .arg("-c")
            .arg(&script)
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(_) => Some(write_fd),
            Err(_) => {
                unsafe { libc::close(write_fd) };
                None
            }
        }
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
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS, RtlCaptureStackBackTrace,
        SetUnhandledExceptionFilter,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::Sleep;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    static WRITE_HANDLE: AtomicUsize = AtomicUsize::new(0);
    static PREV_FILTER: AtomicUsize = AtomicUsize::new(0);
    static IMAGE_BASE: AtomicUsize = AtomicUsize::new(0);

    type FilterFn = unsafe extern "system" fn(*const EXCEPTION_POINTERS) -> i32;

    pub fn install(webhook: String) {
        if !spawn_helper(&webhook) {
            return;
        }
        IMAGE_BASE.store(unsafe { GetModuleHandleW(core::ptr::null()) } as usize, Ordering::Release);
        // Chain to whatever was already registered (Roblox's own crash handler)
        // so their reporting still runs after ours.
        let previous = unsafe { SetUnhandledExceptionFilter(Some(handler)) };
        PREV_FILTER.store(previous.map(|f| f as usize).unwrap_or(0), Ordering::Release);
    }

    fn spawn_helper(webhook: &str) -> bool {
        let script = format!(
            "while($true){{ $l=[Console]::In.ReadLine(); if($l -eq $null){{break}}; \
             try{{ Invoke-RestMethod -Uri '{webhook}' -Method Post -ContentType 'application/json' \
             -Body ('{{\"content\":\"' + $l + '\"}}') }}catch{{}} }}"
        );
        let mut child = match Command::new("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            Ok(child) => child,
            Err(_) => return false,
        };
        let Some(stdin) = child.stdin.take() else { return false };
        WRITE_HANDLE.store(stdin.into_raw_handle() as usize, Ordering::Release);
        std::mem::forget(child);
        true
    }

    unsafe extern "system" fn handler(info: *const EXCEPTION_POINTERS) -> i32 {
        let (code, address) = unsafe {
            info.as_ref()
                .and_then(|p| p.ExceptionRecord.as_ref())
                .map(|record| (record.ExceptionCode as u64, record.ExceptionAddress as usize))
                .unwrap_or((0, 0))
        };

        let mut buf = [0u8; 768];
        let mut len = 0;
        len = append(&mut buf, len, b"CRASH exception=0x");
        len += write_uint(&mut buf[len..], code, 16);
        len = append(&mut buf, len, b" addr=0x");
        len += write_uint(&mut buf[len..], address as u64, 16);
        len = append(&mut buf, len, b" base=0x");
        len += write_uint(&mut buf[len..], IMAGE_BASE.load(Ordering::Acquire) as u64, 16);

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
