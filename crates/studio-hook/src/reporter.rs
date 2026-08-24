use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
const CONFIG_PATH: &str = "/Users/Shared/rbx-theme-set/Logger.json";
#[cfg(target_os = "windows")]
const CONFIG_PATH: &str = r"C:\Users\Public\rbxthemeset\Logger.json";

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const MIN_POST_INTERVAL: Duration = Duration::from_millis(1500);
const MAX_BATCH: usize = 20;
const MAX_CONTENT: usize = 1900;

static SENDER: OnceLock<Sender<String>> = OnceLock::new();

pub fn init() {
    let Some((enabled, webhook)) = load_config() else { return };
    if !enabled || webhook.is_empty() {
        return;
    }

    let (tx, rx) = mpsc::channel::<String>();
    if SENDER.set(tx).is_err() {
        return;
    }
    let hook = webhook.clone();
    std::thread::spawn(move || pump(rx, hook));

    install_panic_hook();
    crash::install(webhook);
    report(&format!("logger online (pid {})", std::process::id()));
}

/// Forwards a line to the webhook if logging is enabled
pub fn report(message: &str) {
    if let Some(tx) = SENDER.get() {
        let _ = tx.send(message.to_owned());
    }
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

fn pump(rx: Receiver<String>, webhook: String) {
    let mut last_post = Instant::now() - MIN_POST_INTERVAL;
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        while batch.len() < MAX_BATCH {
            match rx.recv_timeout(Duration::from_millis(300)) {
                Ok(line) => batch.push(line),
                Err(_) => break,
            }
        }
        let wait = MIN_POST_INTERVAL.saturating_sub(last_post.elapsed());
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
        post(&webhook, &batch.join("\n"));
        last_post = Instant::now();
    }
}

pub(crate) fn post(webhook: &str, content: &str) {
    let mut content = content;
    if content.len() > MAX_CONTENT {
        content = &content[content.len() - MAX_CONTENT..];
    }
    let body = serde_json::json!({ "content": content }).to_string();
    let _ = ureq::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .post(webhook)
        .set("Content-Type", "application/json")
        .send_string(&body);
}

#[cfg(unix)]
mod crash {
    use core::ffi::c_void;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicI32, Ordering};

    static WRITE_FD: AtomicI32 = AtomicI32::new(-1);

    const SIGNALS: [i32; 5] = [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE, libc::SIGABRT];

    pub fn install(webhook: String) {
        let Some(write_fd) = spawn_helper(&webhook) else { return };
        WRITE_FD.store(write_fd, Ordering::Release);

        unsafe {
            let stack_size = libc::SIGSTKSZ.max(64 * 1024);
            let mem = libc::malloc(stack_size);
            if !mem.is_null() {
                let stack = libc::stack_t { ss_sp: mem, ss_flags: 0, ss_size: stack_size };
                libc::sigaltstack(&stack, core::ptr::null_mut());
            }
            for signal in SIGNALS {
                let mut action: libc::sigaction = core::mem::zeroed();
                action.sa_sigaction = handler as *const () as usize;
                action.sa_flags = libc::SA_ONSTACK | libc::SA_SIGINFO | libc::SA_NODEFER;
                libc::sigemptyset(&mut action.sa_mask);
                libc::sigaction(signal, &action, core::ptr::null_mut());
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

    extern "C" fn handler(signal: i32, info: *mut libc::siginfo_t, _ctx: *mut c_void) {
        let fault = unsafe { info.as_ref() }.map(|i| unsafe { i.si_addr() } as usize).unwrap_or(0);

        // Async-signal-safe formatting into a stack buffer: "CRASH signal=NN addr=0x...".
        let mut buf = [0u8; 96];
        let mut len = 0;
        for &b in b"CRASH signal=" {
            buf[len] = b;
            len += 1;
        }
        len += write_uint(&mut buf[len..], signal as u64, 10);
        for &b in b" addr=0x" {
            buf[len] = b;
            len += 1;
        }
        len += write_uint(&mut buf[len..], fault as u64, 16);
        buf[len] = b'\n';
        len += 1;

        let fd = WRITE_FD.load(Ordering::Acquire);
        if fd >= 0 {
            unsafe { libc::write(fd, buf.as_ptr() as *const c_void, len) };
            // Hold the crashing thread briefly so the helper can curl the report
            // out before the process dies.
            let delay = libc::timespec { tv_sec: 3, tv_nsec: 0 };
            unsafe { libc::nanosleep(&delay, core::ptr::null_mut()) };
        }

        unsafe {
            let mut action: libc::sigaction = core::mem::zeroed();
            action.sa_sigaction = libc::SIG_DFL;
            libc::sigaction(signal, &action, core::ptr::null_mut());
            libc::raise(signal);
        }
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
        EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS, SetUnhandledExceptionFilter,
    };
    use windows_sys::Win32::System::Threading::Sleep;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    static WRITE_HANDLE: AtomicUsize = AtomicUsize::new(0);

    pub fn install(webhook: String) {
        if !spawn_helper(&webhook) {
            return;
        }
        unsafe { SetUnhandledExceptionFilter(Some(handler)) };
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

        let mut buf = [0u8; 96];
        let mut len = 0;
        for &b in b"CRASH exception=0x" {
            buf[len] = b;
            len += 1;
        }
        len += write_uint(&mut buf[len..], code, 16);
        for &b in b" addr=0x" {
            buf[len] = b;
            len += 1;
        }
        len += write_uint(&mut buf[len..], address as u64, 16);
        buf[len] = b'\n';
        len += 1;

        let handle = WRITE_HANDLE.load(Ordering::Acquire);
        if handle != 0 {
            let mut written = 0u32;
            unsafe {
                WriteFile(handle as *mut c_void, buf.as_ptr(), len as u32, &mut written, core::ptr::null_mut());
                Sleep(3000);
            }
        }
        EXCEPTION_CONTINUE_SEARCH
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
