use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::mem;
use crate::vm::exec::{self, Primitives};

#[cfg(target_os = "macos")]
fn file(name: &str) -> PathBuf {
    PathBuf::from("/Users/Shared/rbx-theme-set").join(name)
}

#[cfg(not(target_os = "macos"))]
fn file(name: &str) -> PathBuf {
    crate::log_dir().join(name)
}

static LAST_EVAL_MTIME: AtomicU64 = AtomicU64::new(0);
static LAST_POKE_MTIME: AtomicU64 = AtomicU64::new(0);

fn mtime_secs(path: &std::path::Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    modified.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn tick(lua_state: usize, primitives: &Primitives) {
    run_eval(lua_state, primitives);
    run_poke(lua_state);
}

fn run_eval(lua_state: usize, primitives: &Primitives) {
    let request = file("LiveEval.luau");
    let Some(mtime) = mtime_secs(&request) else { return };
    if LAST_EVAL_MTIME.swap(mtime, Ordering::Relaxed) == mtime {
        return;
    }
    let Ok(source) = std::fs::read_to_string(&request) else { return };
    let body = match exec::run(lua_state, primitives, &source, "=LiveEval") {
        Ok(values) => values.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n"),
        Err(err) => format!("error: {err:?}"),
    };
    let _ = std::fs::write(file("LiveEval.out"), format!("[{}] lua_state={lua_state:#x}\n{body}\n", now_secs()));
}

fn parse_hex(token: &str) -> Option<usize> {
    let token = token.trim();
    let stripped = token.strip_prefix("0x").unwrap_or(token);
    usize::from_str_radix(stripped, 16).ok()
}

/// Processes memory commands from `Poke.txt` against the live process, one per line,
/// writing results to `Poke.out`. `ctx` prints the current thread, its userdata and
/// global; `r <addr> <len>` dumps bytes; `w <addr> <u64hex>` writes a word; `p <addr>`
/// reads a pointer. Lets the caps layout be probed without rebuilding.
fn run_poke(lua_state: usize) {
    let poke = file("Poke.txt");
    let Some(mtime) = mtime_secs(&poke) else { return };
    if LAST_POKE_MTIME.swap(mtime, Ordering::Relaxed) == mtime {
        return;
    }
    let Ok(text) = std::fs::read_to_string(&poke) else { return };
    let mut out = format!("[{}] lua_state={lua_state:#x}\n", now_secs());
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        out.push_str(&format!("> {line}\n"));
        match parts.as_slice() {
            ["ctx"] => {
                let extra = mem::read_ptr(lua_state + 0x70).unwrap_or(0);
                let global = mem::read_ptr(lua_state + 0x28).unwrap_or(0);
                out.push_str(&format!("  L={lua_state:#x} extra={extra:#x} global={global:#x}\n"));
            }
            ["r", addr, len] => {
                if let (Some(a), Some(n)) = (parse_hex(addr), parse_hex(len)) {
                    for off in (0..n).step_by(8) {
                        let v = mem::read::<u64>(a + off).unwrap_or(0);
                        out.push_str(&format!("  +{off:#05x} {v:#018x}\n"));
                    }
                }
            }
            ["p", addr] => {
                if let Some(a) = parse_hex(addr) {
                    let v = mem::read_ptr(a).unwrap_or(0);
                    out.push_str(&format!("  *{a:#x} = {v:#x}\n"));
                }
            }
            ["w", addr, val] => {
                if let (Some(a), Some(v)) = (parse_hex(addr), parse_hex(val)) {
                    let ok = mem::write::<u64>(a, v as u64).is_ok();
                    out.push_str(&format!("  wrote {v:#x} -> {a:#x} ok={ok}\n"));
                }
            }
            _ => out.push_str("  ? unknown command\n"),
        }
    }
    let _ = std::fs::write(file("Poke.out"), out);
}
