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

#[derive(Debug)]
pub enum InstallError {
    Resolve(ResolveError),
    NoVtables,
    NoSlots,
}

unsafe extern "C" fn hooked_step(job: *mut c_void, stats: *mut c_void) -> *mut c_void {
    crate::guard("step", || observe(job as usize));

    let original = ORIGINAL_STEP.load(Ordering::Acquire);
    debug_assert!(original != 0, "hook installed before the original was recorded");
    let original: StepFn = unsafe { core::mem::transmute(original) };
    unsafe { original(job, stats) }
}

fn observe(job: usize) {
    let Ok(mut slot) = DISCOVERY.try_lock() else { return };
    let Some(discovery) = slot.as_mut() else { return };

    match discovery.on_step(job, Instant::now()) {
        Event::Became(ready) => crate::log(&format!(
            "capture ready: job={:#x} datamodel={:#x} scriptcontext={:#x} lua_state={:#x}",
            ready.job, ready.data_model, ready.script_context, ready.lua_state
        )),
        Event::Dropped(why) => crate::log(&format!("capture dropped: {why}")),
        _ => {}
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
