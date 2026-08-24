#[cfg(target_os = "macos")]
const CONFIG_PATH: &str = "/Users/Shared/rbx-theme-set/EditorBackground.json";
#[cfg(target_os = "windows")]
const CONFIG_PATH: &str = r"C:\Users\Public\rbxthemeset\EditorBackground.json";

const DEFAULT_OPACITY: f64 = 0.15;

#[derive(Debug, Clone)]
struct Config {
    image: String,
    opacity: f64,
}

pub fn init() {
    let Some(config) = load_config() else { return };
    imp::start(config);
}

fn load_config() -> Option<Config> {
    let text = std::fs::read_to_string(CONFIG_PATH).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    if !json.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true) {
        crate::log("editor-bg: disabled by config");
        return None;
    }
    let image = json.get("image").and_then(|v| v.as_str())?.to_owned();
    if image.is_empty() {
        return None;
    }
    let opacity = json.get("opacity").and_then(|v| v.as_f64()).unwrap_or(DEFAULT_OPACITY).clamp(0.0, 1.0);
    Some(Config { image, opacity })
}

#[cfg(target_os = "macos")]
mod imp {
    use core::ffi::{CStr, c_char, c_void};
    use std::sync::Mutex;

    use super::Config;

    const PAINT_SLOT: usize = 30;
    const PAINTDEVICE_SUBOBJECT_OFFSET: usize = 16;
    const MAX_HOOKED: usize = 16;
    const LIST_ARRAY_OFFSET: isize = 16;

    #[repr(C, align(16))]
    #[derive(Clone, Copy)]
    struct PixmapBuf([u8; 128]);

    #[repr(C)]
    struct QSizeRaw {
        w: i32,
        h: i32,
    }

    #[repr(C)]
    struct QRectRaw {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
    }

    // Qt returns QString / QList by value; both have non-trivial destructors, so the
    // C++ ABI returns them via the sret register (x8). A >16-byte return type forces
    // Rust to use sret too; the wrapped value lands in the first word.
    #[repr(C)]
    struct Owned {
        value: *const c_void,
        _pad: [u64; 2],
    }

    type AllWidgetsFn = unsafe extern "C" fn() -> Owned;
    type ClassNameFn = unsafe extern "C" fn(*const c_void) -> *const c_char;
    type MetaObjectFn = unsafe extern "C" fn(*const c_void) -> *const c_void;
    type FromUtf8Fn = unsafe extern "C" fn(*const c_char, i32) -> Owned;
    type ViewportFn = unsafe extern "C" fn(*const c_void) -> *const c_void;
    type PixmapCtorFn = unsafe extern "C" fn(*mut PixmapBuf, *const c_void, *const c_char, u32);
    type PixmapDimFn = unsafe extern "C" fn(*const PixmapBuf) -> i32;
    type PixmapDtorFn = unsafe extern "C" fn(*mut PixmapBuf);
    type PixmapScaledFn = unsafe extern "C" fn(*const PixmapBuf, *const QSizeRaw, i32, i32) -> PixmapBuf;
    type PainterCtorFn = unsafe extern "C" fn(*mut c_void, *const c_void);
    type PainterDtorFn = unsafe extern "C" fn(*mut c_void);
    type DrawPixmapRectFn = unsafe extern "C" fn(*mut c_void, *const [f64; 4], *const PixmapBuf, *const [f64; 4]);
    type SetOpacityFn = unsafe extern "C" fn(*mut c_void, f64);
    type FrameGeometryFn = unsafe extern "C" fn(*const c_void) -> QRectRaw;
    type PaintFn = unsafe extern "C" fn(*mut c_void, *mut c_void);

    #[derive(Clone, Copy)]
    struct Qt {
        all_widgets: AllWidgetsFn,
        class_name: ClassNameFn,
        viewport: ViewportFn,
        pixmap_width: PixmapDimFn,
        pixmap_height: PixmapDimFn,
        pixmap_dtor: PixmapDtorFn,
        pixmap_scaled: PixmapScaledFn,
        painter_ctor: PainterCtorFn,
        painter_dtor: PainterDtorFn,
        draw_pixmap: DrawPixmapRectFn,
        set_opacity: SetOpacityFn,
        frame_geometry: FrameGeometryFn,
    }

    struct Hook {
        qt: Qt,
        pixmap: PixmapBuf,
        opacity: f64,
        scaled: Option<(i32, i32, PixmapBuf)>,
        hooked: Vec<(usize, usize)>,
    }

    // Safety: only ever touched on the main (Qt UI) thread.
    unsafe impl Send for Hook {}

    static HOOK: Mutex<Option<Hook>> = Mutex::new(None);

    unsafe extern "C" {
        static _dispatch_main_q: c_void;
        fn dispatch_async(queue: *const c_void, block: &block2::Block<dyn Fn()>);
        fn dispatch_after(when: u64, queue: *const c_void, block: &block2::Block<dyn Fn()>);
        fn dispatch_time(base: u64, delta: i64) -> u64;
    }

    const DISPATCH_TIME_NOW: u64 = 0;
    const NSEC_PER_SEC: i64 = 1_000_000_000;

    fn page_size() -> usize {
        unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
    }

    unsafe fn resolve<T: Copy>(name: &CStr) -> Option<T> {
        let sym = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        if sym.is_null() {
            return None;
        }
        Some(unsafe { core::mem::transmute_copy::<*mut c_void, T>(&sym) })
    }

    pub fn start(config: Config) {
        let block = block2::RcBlock::new(move || {
            let config = config.clone();
            crate::guard("editor-bg-setup", move || setup(config));
        });
        unsafe { dispatch_async(&_dispatch_main_q as *const c_void, &block) };
    }

    fn setup(config: Config) {
        let qt = unsafe {
            Qt {
                all_widgets: match resolve(c"_ZN12QApplication10allWidgetsEv") {
                    Some(f) => f,
                    None => return missing(),
                },
                class_name: match resolve(c"_ZNK11QMetaObject9classNameEv") {
                    Some(f) => f,
                    None => return missing(),
                },
                viewport: match resolve(c"_ZNK19QAbstractScrollArea8viewportEv") {
                    Some(f) => f,
                    None => return missing(),
                },
                pixmap_width: match resolve(c"_ZNK7QPixmap5widthEv") {
                    Some(f) => f,
                    None => return missing(),
                },
                pixmap_height: match resolve(c"_ZNK7QPixmap6heightEv") {
                    Some(f) => f,
                    None => return missing(),
                },
                pixmap_dtor: match resolve(c"_ZN7QPixmapD1Ev") {
                    Some(f) => f,
                    None => return missing(),
                },
                pixmap_scaled: match resolve(c"_ZNK7QPixmap6scaledERK5QSizeN2Qt15AspectRatioModeENS3_18TransformationModeE") {
                    Some(f) => f,
                    None => return missing(),
                },
                painter_ctor: match resolve(c"_ZN8QPainterC1EP12QPaintDevice") {
                    Some(f) => f,
                    None => return missing(),
                },
                painter_dtor: match resolve(c"_ZN8QPainterD1Ev") {
                    Some(f) => f,
                    None => return missing(),
                },
                draw_pixmap: match resolve(c"_ZN8QPainter10drawPixmapERK6QRectFRK7QPixmapS2_") {
                    Some(f) => f,
                    None => return missing(),
                },
                set_opacity: match resolve(c"_ZN8QPainter10setOpacityEd") {
                    Some(f) => f,
                    None => return missing(),
                },
                frame_geometry: match resolve(c"_ZNK7QWidget13frameGeometryEv") {
                    Some(f) => f,
                    None => return missing(),
                },
            }
        };
        let Some(pixmap_ctor) = (unsafe {
            resolve::<PixmapCtorFn>(c"_ZN7QPixmapC1ERK7QStringPKc6QFlagsIN2Qt19ImageConversionFlagEE")
        }) else {
            return missing();
        };
        let Some(from_utf8) = (unsafe { resolve::<FromUtf8Fn>(c"_ZN7QString15fromUtf8_helperEPKci") }) else {
            return missing();
        };

        let mut pixmap = PixmapBuf([0; 128]);
        let qstring = unsafe { from_utf8(config.image.as_ptr() as *const c_char, config.image.len() as i32) }.value;
        unsafe { pixmap_ctor(&mut pixmap, &qstring as *const *const c_void as *const c_void, core::ptr::null(), 0) };
        let (pw, ph) = unsafe { ((qt.pixmap_width)(&pixmap), (qt.pixmap_height)(&pixmap)) };
        if pw <= 0 || ph <= 0 {
            crate::log("editor-bg: image failed to load");
            return;
        }

        *HOOK.lock().unwrap_or_else(|p| p.into_inner()) = Some(Hook {
            qt,
            pixmap,
            opacity: config.opacity,
            scaled: None,
            hooked: Vec::new(),
        });
        crate::log(&format!("editor-bg: loaded {}x{} opacity={:.2}", pw, ph, config.opacity));
        try_install();
    }

    fn missing() {
        crate::log("editor-bg: a required Qt symbol was missing, not installing");
    }

    fn schedule_retry() {
        let block = block2::RcBlock::new(|| {
            crate::guard("editor-bg-retry", try_install);
        });
        unsafe {
            let when = dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC);
            dispatch_after(when, &_dispatch_main_q as *const c_void, &block);
        }
    }

    fn try_install() {
        {
            let mut guard = HOOK.lock().unwrap_or_else(|p| p.into_inner());
            let Some(hook) = guard.as_mut() else { return };
            let list = unsafe { (hook.qt.all_widgets)() }.value;
            if !list.is_null() {
                scan_widgets(hook, list);
            }
        }
        schedule_retry();
    }

    fn scan_widgets(hook: &mut Hook, list: *const c_void) {
        let begin = unsafe { *(list.byte_offset(8) as *const i32) };
        let end = unsafe { *(list.byte_offset(12) as *const i32) };
        let array = unsafe { list.byte_offset(LIST_ARRAY_OFFSET) as *const *const c_void };
        for index in begin..end {
            let widget = unsafe { *array.offset(index as isize) };
            if widget.is_null() {
                continue;
            }
            let Some(name) = class_name_of(hook, widget) else { continue };
            if !name.contains("ScriptEditor") {
                continue;
            }
            if !has_valid_viewport(hook, widget, list, begin, end) {
                continue;
            }
            let vtable = unsafe { *(widget as *const usize) };
            hook_vtable(hook, vtable);
        }
    }

    fn class_name_of(hook: &Hook, widget: *const c_void) -> Option<String> {
        unsafe {
            let vtable = *(widget as *const *const c_void);
            if vtable.is_null() {
                return None;
            }
            let meta_object: MetaObjectFn = core::mem::transmute_copy(&*(vtable as *const *const c_void));
            let meta = meta_object(widget);
            if meta.is_null() {
                return None;
            }
            let name = (hook.qt.class_name)(meta);
            if name.is_null() {
                return None;
            }
            Some(CStr::from_ptr(name).to_string_lossy().into_owned())
        }
    }

    fn has_valid_viewport(hook: &Hook, widget: *const c_void, list: *const c_void, begin: i32, end: i32) -> bool {
        let vp = unsafe { (hook.qt.viewport)(widget) };
        if vp.is_null() || vp == widget {
            return false;
        }
        let array = unsafe { list.byte_offset(LIST_ARRAY_OFFSET) as *const *const c_void };
        (begin..end).any(|index| unsafe { *array.offset(index as isize) } == vp)
    }

    fn hook_vtable(hook: &mut Hook, vtable: usize) {
        if hook.hooked.iter().any(|(v, _)| *v == vtable) || hook.hooked.len() >= MAX_HOOKED {
            return;
        }
        let slot = (vtable + PAINT_SLOT * 8) as *mut usize;
        let original = unsafe { *slot };

        let Some(trampoline) = build_trampoline(paint_detour as *const () as usize) else { return };

        let page_size = page_size();
        let page_start = vtable & !(page_size - 1);
        let slot_end = slot as usize + 8;
        let prot_len = (slot_end - page_start).div_ceil(page_size) * page_size;
        if unsafe {
            libc::mprotect(page_start as *mut c_void, prot_len, libc::PROT_READ | libc::PROT_WRITE)
        } != 0
        {
            crate::log("editor-bg: vtable mprotect failed");
            return;
        }
        unsafe { *slot = trampoline };
        hook.hooked.push((vtable, original));
        crate::log(&format!("editor-bg: hooked vtable={vtable:#x} (total {})", hook.hooked.len()));
    }

    fn build_trampoline(target: usize) -> Option<usize> {
        let page_size = page_size();
        let page = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                page_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if page == libc::MAP_FAILED {
            return None;
        }
        unsafe {
            let bytes = page as *mut u8;
            core::ptr::write(bytes as *mut u32, 0x5800_0051); // ldr x17, #8
            core::ptr::write(bytes.add(4) as *mut u32, 0xd61f_0220); // br x17
            core::ptr::write(bytes.add(8) as *mut usize, target);
            libc::mprotect(page, page_size, libc::PROT_READ | libc::PROT_EXEC);
        }
        Some(page as usize)
    }

    unsafe extern "C" fn paint_detour(self_: *mut c_void, event: *mut c_void) {
        crate::guard("editor-bg-paint", || paint(self_, event));
    }

    fn paint(self_: *mut c_void, event: *mut c_void) {
        let mut guard = HOOK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(hook) = guard.as_mut() else { return };

        let vtable = unsafe { *(self_ as *const usize) };
        if let Some((_, original)) = hook.hooked.iter().find(|(v, _)| *v == vtable) {
            let original: PaintFn = unsafe { core::mem::transmute(*original) };
            unsafe { original(self_, event) };
        }

        let widget = {
            let vp = unsafe { (hook.qt.viewport)(self_) };
            if vp.is_null() { self_ as *const c_void } else { vp }
        };

        let pw = unsafe { (hook.qt.pixmap_width)(&hook.pixmap) };
        let ph = unsafe { (hook.qt.pixmap_height)(&hook.pixmap) };
        if pw <= 0 || ph <= 0 {
            return;
        }

        let (mut vw, mut vh) = (2000.0_f64, 2000.0_f64);
        let geo = unsafe { (hook.qt.frame_geometry)(widget) };
        let w = (geo.x2 - geo.x1 + 1) as f64;
        let h = (geo.y2 - geo.y1 + 1) as f64;
        if w > 0.0 && h > 0.0 && w <= 16384.0 && h <= 16384.0 {
            vw = w;
            vh = h;
        }

        let scale = (vw / pw as f64).max(vh / ph as f64);
        let sw = ((pw as f64 * scale + 0.5) as i32).clamp(1, 16384);
        let sh = ((ph as f64 * scale + 0.5) as i32).clamp(1, 16384);

        let stale = !matches!(hook.scaled, Some((cw, ch, _)) if cw == sw && ch == sh);
        if stale {
            if let Some((_, _, mut old)) = hook.scaled.take() {
                unsafe { (hook.qt.pixmap_dtor)(&mut old) };
            }
            let target = QSizeRaw { w: sw, h: sh };
            let scaled = unsafe { (hook.qt.pixmap_scaled)(&hook.pixmap, &target, 0, 1) };
            hook.scaled = Some((sw, sh, scaled));
        }

        let (draw_w, draw_h, source) = match &hook.scaled {
            Some((cw, ch, buf)) => (*cw, *ch, buf as *const PixmapBuf),
            None => (pw, ph, &hook.pixmap as *const PixmapBuf),
        };

        let device = unsafe { widget.byte_offset(PAINTDEVICE_SUBOBJECT_OFFSET as isize) };
        let mut painter = [0u8; 64];
        unsafe { (hook.qt.painter_ctor)(painter.as_mut_ptr() as *mut c_void, device) };
        unsafe { (hook.qt.set_opacity)(painter.as_mut_ptr() as *mut c_void, hook.opacity) };

        let dest = [(vw - draw_w as f64) / 2.0, (vh - draw_h as f64) / 2.0, draw_w as f64, draw_h as f64];
        let src = [0.0, 0.0, draw_w as f64, draw_h as f64];
        unsafe { (hook.qt.draw_pixmap)(painter.as_mut_ptr() as *mut c_void, &dest, source, &src) };
        unsafe { (hook.qt.painter_dtor)(painter.as_mut_ptr() as *mut c_void) };
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use core::ffi::{CStr, c_char, c_void};
    use std::sync::Mutex;

    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        IMAGE_DIRECTORY_ENTRY_EXPORT, IMAGE_EXPORT_DIRECTORY, IMAGE_NT_HEADERS64, UnDecorateSymbolName,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
    use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};
    use windows_sys::Win32::System::SystemServices::IMAGE_DOS_HEADER;
    use windows_sys::Win32::UI::WindowsAndMessaging::SetTimer;

    use super::Config;

    const PAINT_SLOT: usize = 46;
    const PAINTDEVICE_SUBOBJECT_OFFSET: usize = 16;
    const MAX_HOOKED: usize = 16;
    const UNDNAME_COMPLETE: u32 = 0x0000;

    #[repr(C)]
    struct QSizeRaw {
        w: i32,
        h: i32,
    }

    #[repr(C)]
    struct QRectRaw {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
    }

    type AllWidgetsFn = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
    type ClassNameFn = unsafe extern "C" fn(*const c_void) -> *const c_char;
    type MetaObjectFn = unsafe extern "C" fn(*const c_void) -> *const c_void;
    type ViewportFn = unsafe extern "C" fn(*const c_void) -> *const c_void;
    type FromUtf8Fn = unsafe extern "C" fn(*mut c_void, *const c_char, i32);
    type PixmapCtorFn = unsafe extern "C" fn(*mut c_void, *const c_void, *const c_char, u32);
    type PixmapDimFn = unsafe extern "C" fn(*const c_void) -> i32;
    type PixmapDtorFn = unsafe extern "C" fn(*mut c_void);
    type PixmapScaledFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_void, i32, i32);
    type PainterCtorFn = unsafe extern "C" fn(*mut c_void, *const c_void);
    type PainterDtorFn = unsafe extern "C" fn(*mut c_void);
    type SetOpacityFn = unsafe extern "C" fn(*mut c_void, f64);
    type DrawPixmapRectFn = unsafe extern "C" fn(*mut c_void, *const [f64; 4], *const c_void, *const [f64; 4]);
    type FrameGeometryFn = unsafe extern "C" fn(*const c_void, *mut QRectRaw);
    type PaintFn = unsafe extern "C" fn(*mut c_void, *mut c_void);

    #[derive(Clone, Copy)]
    struct Qt {
        all_widgets: AllWidgetsFn,
        class_name: ClassNameFn,
        viewport: ViewportFn,
        from_utf8: FromUtf8Fn,
        pixmap_ctor: PixmapCtorFn,
        pixmap_width: PixmapDimFn,
        pixmap_height: PixmapDimFn,
        pixmap_dtor: PixmapDtorFn,
        pixmap_scaled: PixmapScaledFn,
        painter_ctor: PainterCtorFn,
        painter_dtor: PainterDtorFn,
        set_opacity: SetOpacityFn,
        draw_pixmap: DrawPixmapRectFn,
        frame_geometry: FrameGeometryFn,
    }

    struct Win {
        image: String,
        opacity: f64,
        qt: Option<Qt>,
        pixmap: Option<[u8; 128]>,
        scaled: Option<(i32, i32, [u8; 128])>,
        hooked: Vec<(usize, usize)>,
    }

    // Safety: only ever touched on the Qt UI thread (the message-loop timer + paint).
    unsafe impl Send for Win {}

    static STATE: Mutex<Option<Win>> = Mutex::new(None);

    pub fn start(config: Config) {
        *STATE.lock().unwrap_or_else(|p| p.into_inner()) = Some(Win {
            image: config.image,
            opacity: config.opacity,
            qt: None,
            pixmap: None,
            scaled: None,
            hooked: Vec::new(),
        });
        unsafe { SetTimer(0 as HWND, 0, 1000, Some(retry)) };
    }

    unsafe extern "system" fn retry(_: HWND, _: u32, _: usize, _: u32) {
        crate::guard("editor-bg-retry", || {
            let mut guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
            let Some(win) = guard.as_mut() else { return };
            if win.qt.is_none() {
                win.qt = resolve_symbols();
            }
            let Some(qt) = win.qt else { return };
            if win.pixmap.is_none() {
                win.pixmap = load_pixmap(&qt, &win.image);
            }
            if win.pixmap.is_some() {
                scan_and_hook(win);
            }
        });
    }

    fn load_pixmap(qt: &Qt, image: &str) -> Option<[u8; 128]> {
        let mut qstring = [0u8; 16];
        unsafe {
            (qt.from_utf8)(qstring.as_mut_ptr() as *mut c_void, image.as_ptr() as *const c_char, image.len() as i32)
        };
        let mut pixmap = [0u8; 128];
        unsafe {
            (qt.pixmap_ctor)(pixmap.as_mut_ptr() as *mut c_void, qstring.as_ptr() as *const c_void, core::ptr::null(), 0)
        };
        let pw = unsafe { (qt.pixmap_width)(pixmap.as_ptr() as *const c_void) };
        let ph = unsafe { (qt.pixmap_height)(pixmap.as_ptr() as *const c_void) };
        if pw > 0 && ph > 0 {
            crate::log(&format!("editor-bg: loaded {pw}x{ph}"));
            Some(pixmap)
        } else {
            None
        }
    }

    fn scan_and_hook(win: &mut Win) {
        let Some(qt) = win.qt else { return };
        let mut storage: *mut c_void = core::ptr::null_mut();
        let ret = unsafe { (qt.all_widgets)(&mut storage as *mut _ as *mut c_void) };
        let list = unsafe { *(ret as *const *const c_void) };
        if list.is_null() {
            return;
        }
        let begin = unsafe { *(list.byte_offset(8) as *const i32) };
        let end = unsafe { *(list.byte_offset(12) as *const i32) };
        let array = unsafe { list.byte_offset(16) as *const *const c_void };
        for index in begin..end {
            let widget = unsafe { *array.offset(index as isize) };
            if widget.is_null() {
                continue;
            }
            let Some(name) = class_name_of(&qt, widget) else { continue };
            if !name.contains("ScriptEditor") {
                continue;
            }
            let vp = unsafe { (qt.viewport)(widget) };
            if vp.is_null() || vp == widget {
                continue;
            }
            if !(begin..end).any(|i| unsafe { *array.offset(i as isize) } == vp) {
                continue;
            }
            let vtable = unsafe { *(widget as *const usize) };
            hook_vtable(win, vtable);
        }
    }

    fn class_name_of(qt: &Qt, widget: *const c_void) -> Option<String> {
        unsafe {
            let vtable = *(widget as *const *const c_void);
            if vtable.is_null() {
                return None;
            }
            let meta_object: MetaObjectFn = core::mem::transmute_copy(&*(vtable as *const *const c_void));
            let meta = meta_object(widget);
            if meta.is_null() {
                return None;
            }
            let name = (qt.class_name)(meta);
            if name.is_null() {
                return None;
            }
            Some(CStr::from_ptr(name).to_string_lossy().into_owned())
        }
    }

    fn hook_vtable(win: &mut Win, vtable: usize) {
        if win.hooked.iter().any(|(v, _)| *v == vtable) || win.hooked.len() >= MAX_HOOKED {
            return;
        }
        let slot = (vtable + PAINT_SLOT * 8) as *mut usize;
        let original = unsafe { *slot };
        let mut old = 0u32;
        if unsafe { VirtualProtect(slot as *const c_void, 8, PAGE_READWRITE, &mut old) } == 0 {
            return;
        }
        unsafe { *slot = paint_detour as *const () as usize };
        unsafe { VirtualProtect(slot as *const c_void, 8, old, &mut old) };
        win.hooked.push((vtable, original));
        crate::log(&format!("editor-bg: hooked vtable={vtable:#x} (total {})", win.hooked.len()));
    }

    unsafe extern "C" fn paint_detour(self_: *mut c_void, event: *mut c_void) {
        crate::guard("editor-bg-paint", || paint(self_, event));
    }

    fn paint(self_: *mut c_void, event: *mut c_void) {
        let mut guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
        let Some(win) = guard.as_mut() else { return };
        let Some(qt) = win.qt else { return };

        let vtable = unsafe { *(self_ as *const usize) };
        if let Some((_, original)) = win.hooked.iter().find(|(v, _)| *v == vtable) {
            let original: PaintFn = unsafe { core::mem::transmute(*original) };
            unsafe { original(self_, event) };
        }

        let Some(pixmap) = win.pixmap else { return };
        let widget = {
            let vp = unsafe { (qt.viewport)(self_) };
            if vp.is_null() { self_ as *const c_void } else { vp }
        };

        let pw = unsafe { (qt.pixmap_width)(pixmap.as_ptr() as *const c_void) };
        let ph = unsafe { (qt.pixmap_height)(pixmap.as_ptr() as *const c_void) };
        if pw <= 0 || ph <= 0 {
            return;
        }

        let (mut vw, mut vh) = (2000.0_f64, 2000.0_f64);
        let mut geo = QRectRaw { x1: 0, y1: 0, x2: 0, y2: 0 };
        unsafe { (qt.frame_geometry)(widget, &mut geo) };
        let w = (geo.x2 - geo.x1 + 1) as f64;
        let h = (geo.y2 - geo.y1 + 1) as f64;
        if w > 0.0 && h > 0.0 && w <= 16384.0 && h <= 16384.0 {
            vw = w;
            vh = h;
        }

        let scale = (vw / pw as f64).max(vh / ph as f64);
        let sw = ((pw as f64 * scale + 0.5) as i32).clamp(1, 16384);
        let sh = ((ph as f64 * scale + 0.5) as i32).clamp(1, 16384);

        let stale = !matches!(win.scaled, Some((cw, ch, _)) if cw == sw && ch == sh);
        if stale {
            if let Some((_, _, mut old)) = win.scaled.take() {
                unsafe { (qt.pixmap_dtor)(old.as_mut_ptr() as *mut c_void) };
            }
            let target = QSizeRaw { w: sw, h: sh };
            let mut scaled = [0u8; 128];
            unsafe {
                (qt.pixmap_scaled)(
                    pixmap.as_ptr() as *mut c_void,
                    scaled.as_mut_ptr() as *mut c_void,
                    &target as *const QSizeRaw as *const c_void,
                    0,
                    1,
                )
            };
            win.scaled = Some((sw, sh, scaled));
        }

        let (draw_w, draw_h, source) = match &win.scaled {
            Some((cw, ch, buf)) => (*cw, *ch, buf.as_ptr() as *const c_void),
            None => (pw, ph, pixmap.as_ptr() as *const c_void),
        };

        let device = unsafe { widget.byte_offset(PAINTDEVICE_SUBOBJECT_OFFSET as isize) };
        let mut painter = [0u8; 64];
        unsafe { (qt.painter_ctor)(painter.as_mut_ptr() as *mut c_void, device) };
        unsafe { (qt.set_opacity)(painter.as_mut_ptr() as *mut c_void, win.opacity) };

        let dest = [(vw - draw_w as f64) / 2.0, (vh - draw_h as f64) / 2.0, draw_w as f64, draw_h as f64];
        let src = [0.0, 0.0, draw_w as f64, draw_h as f64];
        unsafe { (qt.draw_pixmap)(painter.as_mut_ptr() as *mut c_void, &dest, source, &src) };
        unsafe { (qt.painter_dtor)(painter.as_mut_ptr() as *mut c_void) };
    }

    fn resolve_symbols() -> Option<Qt> {
        let core = load_module(&["Qt5Core.dll", "Qt5Cored.dll"])?;
        let widgets = load_module(&["Qt5Widgets.dll", "Qt5Widgetsd.dll"])?;
        let gui = load_module(&["Qt5Gui.dll", "Qt5Guid.dll"])?;
        unsafe {
            Some(Qt {
                all_widgets: cast(find_export(widgets, &["QApplication::allWidgets"], None)?),
                class_name: cast(find_export(core, &["QMetaObject::className"], None)?),
                viewport: cast(find_export(widgets, &["QAbstractScrollArea::viewport(void)"], None)?),
                from_utf8: cast(find_export(core, &["QString::fromUtf8(", "char const"], None)?),
                pixmap_ctor: cast(find_export(gui, &["QPixmap::QPixmap", "class QString", "ImageConversionFlag"], None)?),
                pixmap_width: cast(find_export(gui, &["QPixmap::width"], None)?),
                pixmap_height: cast(find_export(gui, &["QPixmap::height"], None)?),
                pixmap_dtor: cast(find_export(gui, &["QPixmap::~QPixmap"], None)?),
                pixmap_scaled: cast(find_export(gui, &["QPixmap::scaled", "QSize"], None)?),
                painter_ctor: cast(find_export(gui, &["QPainter::QPainter", "QPaintDevice"], None)?),
                painter_dtor: cast(find_export(gui, &["QPainter::~QPainter"], None)?),
                set_opacity: cast(find_export(gui, &["QPainter::setOpacity"], None)?),
                draw_pixmap: cast(find_export(gui, &["QPainter::drawPixmap", "QPixmap"], Some("QRectF"))?),
                frame_geometry: cast(find_export(widgets, &["QWidget::frameGeometry"], None)?),
            })
        }
    }

    unsafe fn cast<T: Copy>(ptr: *const c_void) -> T {
        unsafe { core::mem::transmute_copy::<*const c_void, T>(&ptr) }
    }

    fn load_module(names: &[&str]) -> Option<usize> {
        for name in names {
            let cname = std::ffi::CString::new(*name).ok()?;
            let handle = unsafe { GetModuleHandleA(cname.as_ptr() as *const u8) };
            if !handle.is_null() {
                return Some(handle as usize);
            }
        }
        None
    }

    fn find_export(module: usize, required: &[&str], must_twice: Option<&str>) -> Option<*const c_void> {
        unsafe {
            let base = module as *const u8;
            let dos = base as *const IMAGE_DOS_HEADER;
            let nt = base.offset((*dos).e_lfanew as isize) as *const IMAGE_NT_HEADERS64;
            let dir = (*nt).OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_EXPORT as usize];
            if dir.VirtualAddress == 0 {
                return None;
            }
            let exports = base.offset(dir.VirtualAddress as isize) as *const IMAGE_EXPORT_DIRECTORY;
            let names = base.offset((*exports).AddressOfNames as isize) as *const u32;
            let ordinals = base.offset((*exports).AddressOfNameOrdinals as isize) as *const u16;
            let functions = base.offset((*exports).AddressOfFunctions as isize) as *const u32;

            let mut buf = [0u8; 1024];
            for i in 0..(*exports).NumberOfNames {
                let mangled = base.offset(*names.offset(i as isize) as isize);
                let written = UnDecorateSymbolName(mangled, buf.as_mut_ptr(), buf.len() as u32, UNDNAME_COMPLETE);
                if written == 0 {
                    continue;
                }
                let undecorated = CStr::from_ptr(buf.as_ptr() as *const c_char).to_string_lossy();
                let all = required.iter().all(|needle| undecorated.contains(needle));
                let twice = must_twice.map(|n| undecorated.matches(n).count() >= 2).unwrap_or(true);
                if all && twice {
                    let rva = *functions.offset(*ordinals.offset(i as isize) as isize);
                    return Some(base.offset(rva as isize) as *const c_void);
                }
            }
            None
        }
    }
}
