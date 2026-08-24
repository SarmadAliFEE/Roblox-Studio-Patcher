use core::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use crate::platform;
use crate::vm::discovery::{Discovery, Event, Vtables};
use crate::vm::resolve::{self, ResolveError};

type StepFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;

static ORIGINAL_STEP: AtomicUsize = AtomicUsize::new(0);
static DISCOVERY: Mutex<Option<Discovery>> = Mutex::new(None);
static PRIMITIVES: Mutex<Option<crate::vm::exec::Primitives>> = Mutex::new(None);
static PROBED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Debug)]
pub enum InstallError {
    Resolve(ResolveError),
    NoVtables,
    NoSlots,
}

unsafe extern "C" fn hooked_step(job: *mut c_void, stats: *mut c_void) -> *mut c_void {
    crate::guard("step", || {
        if let Some(ready) = observe(job as usize) {
            probe(ready);
        }
    });

    let original = ORIGINAL_STEP.load(Ordering::Acquire);
    debug_assert!(original != 0, "hook installed before the original was recorded");
    let original: StepFn = unsafe { core::mem::transmute(original) };
    unsafe { original(job, stats) }
}

fn observe(job: usize) -> Option<crate::vm::discovery::Ready> {
    let Ok(mut slot) = DISCOVERY.try_lock() else { return None };
    let Some(discovery) = slot.as_mut() else { return None };

    let event = discovery.on_step(job, Instant::now());
    if let Event::Live(ready) = event {
        if std::env::var("STUDIO_HOOK_PROBE").is_ok() && !PROBED.swap(true, Ordering::AcqRel) {
            return Some(ready);
        }
    }

    match event {
        Event::Became(ready) => crate::log(&format!(
            "capture ready: job={:#x} datamodel={:#x} scriptcontext={:#x} lua_state={:#x}",
            ready.job, ready.data_model, ready.script_context, ready.lua_state
        )),
        Event::Dropped(why) => crate::log(&format!("capture dropped: {why}")),
        _ => {}
    }
    None
}

fn probe(ready: crate::vm::discovery::Ready) {
    let Ok(slot) = PRIMITIVES.try_lock() else { return };
    let Some(primitives) = slot.as_ref() else { return };

    let source = "return 1 + 1";
    match crate::vm::exec::run(ready.lua_state, primitives, source, "=StudioHookProbe") {
        Ok(values) => {
            let rendered: Vec<String> = values.iter().map(|v| v.to_string()).collect();
            crate::log(&format!("probe ok: [{}]", rendered.join(", ")));
        }
        Err(err) => crate::log(&format!("probe failed: {err:?}")),
    }
}

pub fn ready() -> Option<crate::vm::discovery::Ready> {
    let slot = DISCOVERY.try_lock().ok()?;
    slot.as_ref()?.settled(Instant::now())
}

pub fn install() -> Result<usize, InstallError> {
    let flags = crate::vm::luau::enable_luau_flags();
    crate::log(&format!("luau: enabled {flags} compiler flag(s)"));

    let resolved = resolve::resolve().map_err(InstallError::Resolve)?;

    let (Some(data_model), Some(script_context)) =
        (resolved.data_model_vtable, resolved.script_context_vtable)
    else {
        return Err(InstallError::NoVtables);
    };
    if resolved.jobs.is_empty() {
        return Err(InstallError::NoSlots);
    }

    let vtables = Vtables {
        data_model,
        script_context,
        waiting_hybrid: resolved.waiting_hybrid_vtable,
    };
    *DISCOVERY.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(Discovery::new(vtables));

    if let (Some(load), Some(call)) = (resolved.luau_load, resolved.call_dispatch) {
        *PRIMITIVES.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(crate::vm::exec::Primitives {
                load,
                call,
                new_thread: resolved.lua_newthread,
                security_context_current: resolved.security_context_current,
            });
    } else {
        crate::log("hook: luau primitives unavailable, execution disabled");
    }

    ORIGINAL_STEP.store(resolved.step, Ordering::Release);

    let mut patched = 0;
    for job in &resolved.jobs {
        let target = hooked_step as StepFn as *const () as usize;
        match platform::patch_pointer(job.slot_addr, target) {
            Ok(_) => patched += 1,
            Err(err) => crate::log(&format!("hook: {} slot {:?} not patched", job.rtti, err)),
        }
    }

    if patched == 0 {
        return Err(InstallError::NoSlots);
    }
    Ok(patched)
}
