use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const RECONNECT_INTERVAL: Duration = Duration::from_millis(5000);
const MAX_FRAME: u32 = 1 << 20;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    pub state: String,
    pub details: String,
    pub large_image: String,
    pub large_text: String,
    pub small_image: String,
    pub small_text: String,
    pub button_label: String,
    pub button_url: String,
    pub start_unix: i64,
}

struct Mailbox {
    pending: Mutex<Option<Activity>>,
    signal: Condvar,
}

pub struct Ipc {
    mailbox: &'static Mailbox,
}

impl Ipc {
    pub fn start(client_id: String) -> Ipc {
        let mailbox: &'static Mailbox = Box::leak(Box::new(Mailbox {
            pending: Mutex::new(None),
            signal: Condvar::new(),
        }));
        std::thread::spawn(move || run(client_id, mailbox));
        Ipc { mailbox }
    }

    pub fn set_activity(&self, activity: Activity) {
        *self.mailbox.pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(activity);
        self.mailbox.signal.notify_one();
    }
}

fn run(client_id: String, mailbox: &'static Mailbox) {
    loop {
        let Some(mut conn) = connect() else {
            std::thread::sleep(RECONNECT_INTERVAL);
            continue;
        };
        if handshake(&mut conn, &client_id).is_err() {
            continue;
        }
        crate::log("discord: connected");
        serve(&mut conn, mailbox);
        crate::log("discord: disconnected, will reconnect");
    }
}

fn serve(conn: &mut Connection, mailbox: &'static Mailbox) {
    loop {
        let activity = {
            let mut guard = mailbox.pending.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                if let Some(activity) = guard.take() {
                    break activity;
                }
                guard = mailbox.signal.wait(guard).unwrap_or_else(|p| p.into_inner());
            }
        };
        if send_frame(conn, OP_FRAME, &set_activity_payload(&activity)).is_err() {
            return;
        }
    }
}

fn handshake(conn: &mut Connection, client_id: &str) -> std::io::Result<()> {
    let payload = format!("{{\"v\":1,\"client_id\":\"{}\"}}", escape(client_id));
    send_frame(conn, OP_HANDSHAKE, &payload)?;
    read_frame(conn).map(|_| ())
}

fn set_activity_payload(a: &Activity) -> String {
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let mut assets = String::new();
    if !a.large_image.is_empty() || !a.small_image.is_empty() {
        assets.push_str(",\"assets\":{");
        let mut first = true;
        for (key, text, tag) in [
            (&a.large_image, &a.large_text, "large"),
            (&a.small_image, &a.small_text, "small"),
        ] {
            if key.is_empty() {
                continue;
            }
            if !first {
                assets.push(',');
            }
            first = false;
            assets.push_str(&format!("\"{tag}_image\":\"{}\"", escape(key)));
            if !text.is_empty() {
                assets.push_str(&format!(",\"{tag}_text\":\"{}\"", escape(text)));
            }
        }
        assets.push('}');
    }
    let buttons = if !a.button_label.is_empty() && !a.button_url.is_empty() {
        format!(
            ",\"buttons\":[{{\"label\":\"{}\",\"url\":\"{}\"}}]",
            escape(&a.button_label),
            escape(&a.button_url)
        )
    } else {
        String::new()
    };
    format!(
        "{{\"cmd\":\"SET_ACTIVITY\",\"args\":{{\"pid\":{},\"activity\":{{\"state\":\"{}\",\"details\":\"{}\",\"timestamps\":{{\"start\":{}}}{}{}}}}},\"nonce\":\"{}\"}}",
        std::process::id(),
        escape(&a.state),
        escape(&a.details),
        a.start_unix,
        assets,
        buttons,
        NONCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn send_frame(conn: &mut Connection, opcode: u32, json: &str) -> std::io::Result<()> {
    let mut header = [0u8; 8];
    header[..4].copy_from_slice(&opcode.to_le_bytes());
    header[4..].copy_from_slice(&(json.len() as u32).to_le_bytes());
    conn.write_all(&header)?;
    conn.write_all(json.as_bytes())
}

fn read_frame(conn: &mut Connection) -> std::io::Result<String> {
    let mut header = [0u8; 8];
    conn.read_exact(&mut header)?;
    let len = u32::from_le_bytes(header[4..].try_into().unwrap());
    if len > MAX_FRAME {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut body = vec![0u8; len as usize];
    conn.read_exact(&mut body)?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(target_os = "macos")]
type Connection = std::os::unix::net::UnixStream;

#[cfg(target_os = "macos")]
fn connect() -> Option<Connection> {
    use std::os::unix::net::UnixStream;
    let dirs = [
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::var_os("TMPDIR"),
        Some("/tmp".into()),
    ];
    for dir in dirs.into_iter().flatten() {
        for i in 0..10 {
            let path = std::path::Path::new(&dir).join(format!("discord-ipc-{i}"));
            if let Ok(stream) = UnixStream::connect(&path) {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                return Some(stream);
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
type Connection = std::fs::File;

#[cfg(target_os = "windows")]
fn connect() -> Option<Connection> {
    use std::fs::OpenOptions;
    for i in 0..10 {
        let path = format!(r"\\.\pipe\discord-ipc-{i}");
        if let Ok(pipe) = OpenOptions::new().read(true).write(true).open(&path) {
            return Some(pipe);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_the_characters_json_cannot_carry_raw() {
        assert_eq!(escape("a\"b\\c\n"), "a\\\"b\\\\c\\n");
        assert_eq!(escape("plain"), "plain");
    }

    #[test]
    fn a_payload_carries_state_details_and_a_start_timestamp() {
        let activity = Activity {
            state: "Editing Main".into(),
            details: "My Place".into(),
            start_unix: 1234,
            ..Default::default()
        };
        let payload = set_activity_payload(&activity);
        assert!(payload.contains("\"state\":\"Editing Main\""));
        assert!(payload.contains("\"details\":\"My Place\""));
        assert!(payload.contains("\"start\":1234"));
        assert!(!payload.contains("assets"));
    }

    #[test]
    fn a_button_is_only_emitted_when_both_label_and_url_are_present() {
        let mut activity = Activity { button_label: "View Place".into(), ..Default::default() };
        assert!(!set_activity_payload(&activity).contains("buttons"));
        activity.button_url = "https://example.com".into();
        assert!(set_activity_payload(&activity).contains("\"buttons\""));
    }

    #[test]
    fn assets_carry_only_the_images_that_are_set() {
        let activity = Activity {
            large_image: "logo".into(),
            large_text: "Roblox".into(),
            ..Default::default()
        };
        let payload = set_activity_payload(&activity);
        assert!(payload.contains("\"large_image\":\"logo\""));
        assert!(payload.contains("\"large_text\":\"Roblox\""));
        assert!(!payload.contains("small_image"));
    }
}
