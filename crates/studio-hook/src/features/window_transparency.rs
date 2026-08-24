#[cfg(target_os = "macos")]
const CONFIG_PATH: &str = "/Users/Shared/rbx-theme-set/WindowTransparency.json";
#[cfg(target_os = "windows")]
const CONFIG_PATH: &str = r"C:\Users\Public\rbxthemeset\WindowTransparency.json";

const DEFAULT_OPACITY: f64 = 1.0;
const DEFAULT_STEP: f64 = 0.05;
const DEFAULT_MIN_OPACITY: f64 = 0.2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    pub opacity: f64,
    pub step: f64,
    pub min_opacity: f64,
    pub increase: Hotkey,
    pub decrease: Hotkey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Hotkey {
    pub cmd: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub key: Key,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Plus,
    Minus,
}

impl Default for Key {
    fn default() -> Key {
        Key::Char('\0')
    }
}

pub fn init() {
    let Some(config) = load_config() else { return };
    start(config);
}

fn load_config() -> Option<Config> {
    let text = std::fs::read_to_string(CONFIG_PATH).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    if !json.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true) {
        crate::log("transparency: disabled by config");
        return None;
    }
    let number = |key: &str, fallback: f64| json.get(key).and_then(|v| v.as_f64()).unwrap_or(fallback);
    let min_opacity = number("minOpacity", DEFAULT_MIN_OPACITY).clamp(0.0, 1.0);
    let opacity = number("opacity", DEFAULT_OPACITY).clamp(min_opacity, 1.0);
    let increase = json
        .get("increaseHotkey")
        .and_then(|v| v.as_str())
        .and_then(parse_hotkey)
        .unwrap_or(Hotkey { ctrl: true, key: Key::Plus, ..Default::default() });
    let decrease = json
        .get("decreaseHotkey")
        .and_then(|v| v.as_str())
        .and_then(parse_hotkey)
        .unwrap_or(Hotkey { ctrl: true, key: Key::Minus, ..Default::default() });
    Some(Config { opacity, step: number("step", DEFAULT_STEP), min_opacity, increase, decrease })
}

fn parse_hotkey(spec: &str) -> Option<Hotkey> {
    let parts: Vec<&str> = spec.split('+').filter(|part| !part.is_empty()).collect();
    let (mods, last) = parts.split_at(parts.len().checked_sub(1)?);
    let mut hotkey = Hotkey::default();
    for part in mods {
        match part.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "win" | "windows" => hotkey.cmd = true,
            "option" | "alt" => hotkey.alt = true,
            "control" | "ctrl" => hotkey.ctrl = true,
            "shift" => hotkey.shift = true,
            _ => return None,
        }
    }
    hotkey.key = match last[0].to_ascii_lowercase().as_str() {
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "=" | "+" => Key::Plus,
        "-" => Key::Minus,
        other => Key::Char(other.chars().next()?),
    };
    Some(hotkey)
}

fn persist_opacity(opacity: f64) {
    let Ok(text) = std::fs::read_to_string(CONFIG_PATH) else { return };
    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    if let Some(object) = json.as_object_mut() {
        object.insert("opacity".to_owned(), serde_json::json!(opacity));
        let _ = std::fs::write(CONFIG_PATH, serde_json::to_string_pretty(&json).unwrap_or(text));
    }
}


#[cfg(target_os = "macos")]
mod imp {
    use core::ffi::{CStr, c_char, c_void};
    use std::sync::Mutex;

    use block2::RcBlock;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    use super::{Config, Hotkey, Key};

    const MASK_KEY_DOWN: u64 = 1 << 10;
    const DEVICE_INDEPENDENT_MASK: u64 = 0xFFFF_0000;
    const SHIFT: u64 = 1 << 17;
    const CONTROL: u64 = 1 << 18;
    const OPTION: u64 = 1 << 19;
    const COMMAND: u64 = 1 << 20;

    struct State {
        config: Config,
        opacity: f64,
    }

    static STATE: Mutex<Option<State>> = Mutex::new(None);

    unsafe extern "C" {
        static _dispatch_main_q: c_void;
        fn dispatch_async(queue: *const c_void, block: &block2::Block<dyn Fn()>);
    }

    pub fn start(config: Config) {
        *STATE.lock().unwrap_or_else(|p| p.into_inner()) = Some(State { config, opacity: config.opacity });
        let block = RcBlock::new(|| {
            crate::guard("transparency-setup", setup);
        });
        unsafe { dispatch_async(&_dispatch_main_q as *const c_void, &block) };
    }

    fn setup() {
        apply(current_opacity());
        install_monitor();
        crate::log("transparency: installed");
    }

    fn current_opacity() -> f64 {
        STATE.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(|s| s.opacity).unwrap_or(1.0)
    }

    fn apply(opacity: f64) {
        unsafe {
            let Some(app_class) = AnyClass::get(c"NSApplication") else { return };
            let app: *mut AnyObject = msg_send![app_class, sharedApplication];
            if app.is_null() {
                return;
            }
            let windows: *mut AnyObject = msg_send![app, windows];
            if windows.is_null() {
                return;
            }
            let count: usize = msg_send![windows, count];
            for index in 0..count {
                let window: *mut AnyObject = msg_send![windows, objectAtIndex: index];
                if !window.is_null() {
                    let _: () = msg_send![window, setAlphaValue: opacity];
                }
            }
        }
    }

    fn install_monitor() {
        let handler = RcBlock::new(|event: *mut AnyObject| -> *mut AnyObject {
            if crate::guard("transparency-key", || on_key(event)).unwrap_or(false) {
                core::ptr::null_mut()
            } else {
                event
            }
        });
        unsafe {
            let Some(class) = AnyClass::get(c"NSEvent") else { return };
            let _: *mut AnyObject = msg_send![
                class,
                addLocalMonitorForEventsMatchingMask: MASK_KEY_DOWN,
                handler: &*handler,
            ];
        }
        core::mem::forget(handler);
    }

    fn on_key(event: *mut AnyObject) -> bool {
        let (increase, decrease, step, min) = {
            let guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
            let Some(state) = guard.as_ref() else { return false };
            (state.config.increase, state.config.decrease, state.config.step, state.config.min_opacity)
        };

        let mods = unsafe {
            let raw: u64 = msg_send![event, modifierFlags];
            raw & DEVICE_INDEPENDENT_MASK
        };
        let key_code: u16 = unsafe { msg_send![event, keyCode] };
        let chars = event_chars(event);

        let delta = if matches(&increase, mods, key_code, chars.as_deref()) {
            step
        } else if matches(&decrease, mods, key_code, chars.as_deref()) {
            -step
        } else {
            return false;
        };

        let opacity = {
            let mut guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
            let Some(state) = guard.as_mut() else { return false };
            state.opacity = (state.opacity + delta).clamp(min, 1.0);
            state.opacity
        };
        apply(opacity);
        super::persist_opacity(opacity);
        true
    }

    fn event_chars(event: *mut AnyObject) -> Option<String> {
        unsafe {
            let string: *mut AnyObject = msg_send![event, charactersIgnoringModifiers];
            if string.is_null() {
                return None;
            }
            let utf8: *const c_char = msg_send![string, UTF8String];
            if utf8.is_null() {
                return None;
            }
            Some(CStr::from_ptr(utf8).to_string_lossy().to_lowercase())
        }
    }

    fn matches(hotkey: &Hotkey, mods: u64, key_code: u16, chars: Option<&str>) -> bool {
        let want = modifier_mask(hotkey);
        match hotkey.key {
            Key::Char(expected) => {
                (mods & !SHIFT) == (want & !SHIFT)
                    && chars.map(|c| c == expected.to_string()).unwrap_or(false)
            }
            named => mods == want && key_code == key_code_for(named),
        }
    }

    fn modifier_mask(hotkey: &Hotkey) -> u64 {
        let mut mask = 0;
        if hotkey.cmd {
            mask |= COMMAND;
        }
        if hotkey.alt {
            mask |= OPTION;
        }
        if hotkey.ctrl {
            mask |= CONTROL;
        }
        if hotkey.shift {
            mask |= SHIFT;
        }
        mask
    }

    fn key_code_for(key: Key) -> u16 {
        match key {
            Key::Up => 126,
            Key::Down => 125,
            Key::Left => 123,
            Key::Right => 124,
            Key::Plus => 24,
            Key::Minus => 27,
            Key::Char(_) => u16::MAX,
        }
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::sync::Mutex;

    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_DOWN, VK_LEFT, VK_LWIN, VK_MENU, VK_OEM_MINUS, VK_OEM_PLUS,
        VK_RIGHT, VK_RWIN, VK_SHIFT, VK_UP,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowLongPtrA, GetWindowThreadProcessId, IsWindowVisible,
        SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrA, GWL_EXSTYLE, GWL_STYLE, LWA_ALPHA,
        WS_CAPTION, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
    };

    use super::{Config, Hotkey, Key};

    const POLL_MS: u32 = 50;

    struct State {
        config: Config,
        opacity: f64,
        increase_down: bool,
        decrease_down: bool,
    }

    static STATE: Mutex<Option<State>> = Mutex::new(None);

    pub fn start(config: Config) {
        *STATE.lock().unwrap_or_else(|p| p.into_inner()) = Some(State {
            config,
            opacity: config.opacity,
            increase_down: false,
            decrease_down: false,
        });
        apply(config.opacity);
        unsafe { SetTimer(0 as HWND, 0, POLL_MS, Some(poll)) };
        crate::log("transparency: installed");
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, alpha: LPARAM) -> BOOL {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid != unsafe { GetCurrentProcessId() } || unsafe { IsWindowVisible(hwnd) } == 0 {
            return TRUE;
        }
        let ex_style = unsafe { GetWindowLongPtrA(hwnd, GWL_EXSTYLE) } as u32;
        if ex_style & WS_EX_TOOLWINDOW != 0 {
            return TRUE;
        }
        let style = unsafe { GetWindowLongPtrA(hwnd, GWL_STYLE) } as u32;
        if style & WS_CAPTION == 0 {
            return TRUE;
        }
        if ex_style & WS_EX_LAYERED == 0 {
            unsafe { SetWindowLongPtrA(hwnd, GWL_EXSTYLE, (ex_style | WS_EX_LAYERED) as isize) };
        }
        unsafe { SetLayeredWindowAttributes(hwnd, 0, alpha as u8, LWA_ALPHA) };
        TRUE
    }

    fn apply(opacity: f64) {
        let alpha = (opacity.clamp(0.0, 1.0) * 255.0) as isize;
        unsafe { EnumWindows(Some(enum_proc), alpha) };
    }

    unsafe extern "system" fn poll(_: HWND, _: u32, _: usize, _: u32) {
        crate::guard("transparency-poll", tick);
    }

    fn tick() {
        let opacity = {
            let mut guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
            let Some(state) = guard.as_mut() else { return };

            let increase = key_down(&state.config.increase);
            let decrease = key_down(&state.config.decrease);
            let mut changed = None;
            if increase && !state.increase_down {
                state.opacity = (state.opacity + state.config.step).min(1.0);
                changed = Some(state.opacity);
            }
            if decrease && !state.decrease_down {
                state.opacity = (state.opacity - state.config.step).max(state.config.min_opacity);
                changed = Some(state.opacity);
            }
            state.increase_down = increase;
            state.decrease_down = decrease;
            changed
        };
        if let Some(opacity) = opacity {
            apply(opacity);
            super::persist_opacity(opacity);
        }
    }

    fn pressed(vk: i32) -> bool {
        (unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000) != 0
    }

    fn key_down(hotkey: &Hotkey) -> bool {
        if pressed(VK_CONTROL as i32) != hotkey.ctrl
            || pressed(VK_MENU as i32) != hotkey.alt
            || pressed(VK_SHIFT as i32) != hotkey.shift
            || (pressed(VK_LWIN as i32) || pressed(VK_RWIN as i32)) != hotkey.cmd
        {
            return false;
        }
        pressed(virtual_key(hotkey.key))
    }

    fn virtual_key(key: Key) -> i32 {
        match key {
            Key::Up => VK_UP as i32,
            Key::Down => VK_DOWN as i32,
            Key::Left => VK_LEFT as i32,
            Key::Right => VK_RIGHT as i32,
            Key::Plus => VK_OEM_PLUS as i32,
            Key::Minus => VK_OEM_MINUS as i32,
            Key::Char(c) => c.to_ascii_uppercase() as i32,
        }
    }
}

fn start(config: Config) {
    imp::start(config);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_modifier_plus_named_key() {
        let hk = parse_hotkey("ctrl+up").expect("parses");
        assert!(hk.ctrl && !hk.cmd);
        assert_eq!(hk.key, Key::Up);
    }

    #[test]
    fn parses_a_character_hotkey_with_multiple_modifiers() {
        let hk = parse_hotkey("cmd+shift+a").expect("parses");
        assert!(hk.cmd && hk.shift);
        assert_eq!(hk.key, Key::Char('a'));
    }

    #[test]
    fn equals_and_minus_map_to_the_plus_and_minus_keys() {
        assert_eq!(parse_hotkey("ctrl+=").unwrap().key, Key::Plus);
        assert_eq!(parse_hotkey("alt+-").unwrap().key, Key::Minus);
    }

    #[test]
    fn an_unknown_modifier_is_rejected() {
        assert!(parse_hotkey("hyper+a").is_none());
    }
}
