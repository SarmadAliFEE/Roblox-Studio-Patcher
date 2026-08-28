use core::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use crate::discord::presence::Presence;
use crate::platform;
use crate::vm::discovery::{Discovery, Ready, Vtables};
use crate::vm::exec::Primitives;
use crate::vm::layout::{CapabilityLayout, LuaProbe};
use crate::vm::resolve::{self, ResolveError};

type StepFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;

static ORIGINAL_STEP: AtomicUsize = AtomicUsize::new(0);
static DISCOVERY: Mutex<Option<Discovery>> = Mutex::new(None);
static PRIMITIVES: Mutex<Option<Primitives>> = Mutex::new(None);
static PRESENCE: Mutex<Option<Presence>> = Mutex::new(None);

#[derive(Debug)]
pub enum InstallError {
    Resolve(ResolveError),
    NoVtables,
    NoSlots,
    Unsupported,
}

unsafe extern "C" fn hooked_step(job: *mut c_void, stats: *mut c_void) -> *mut c_void {
    crate::guard("step", || observe(job as usize));

    let original = ORIGINAL_STEP.load(Ordering::Acquire);
    debug_assert!(original != 0, "hook installed before the original was recorded");
    let original: StepFn = unsafe { core::mem::transmute(original) };
    unsafe { original(job, stats) }
}

enum Action {
    Poll(Ready, bool),
    Idle,
    Hold,
}

fn observe(job: usize) {
    let now = Instant::now();
    let probe_state = {
        let Ok(mut slot) = DISCOVERY.try_lock() else { return };
        let Some(discovery) = slot.as_mut() else { return };
        discovery.on_step(job, now);
        discovery.running_probe_due(job, now)
    };
    if let Some(state) = probe_state {
        let running = match PRIMITIVES.try_lock() {
            Ok(slot) => slot.as_ref().map(|primitives| Presence::probe_running(state, primitives)),
            Err(_) => None,
        };
        if let Some(running) = running {
            if let Ok(mut slot) = DISCOVERY.try_lock() {
                if let Some(discovery) = slot.as_mut() {
                    discovery.note_job_running(job, running, now);
                }
            }
        }
    }
    let action = {
        let Ok(mut slot) = DISCOVERY.try_lock() else { return };
        let Some(discovery) = slot.as_mut() else { return };
        match discovery.edit(now) {
            Some(ready) if ready.job == job => Action::Poll(ready, discovery.play_test_active(now)),
            Some(_) => Action::Hold,
            None => Action::Idle,
        }
    };
    match action {
        Action::Poll(ready, play_test) => drive_presence(ready, play_test),
        Action::Idle => drive_idle(),
        Action::Hold => {}
    }
}

fn drive_presence(ready: Ready, play_test: bool) {
    let keep = {
        let Ok(mut presence_slot) = PRESENCE.try_lock() else { return };
        let Some(presence) = presence_slot.as_mut() else { return };
        let Ok(primitives_slot) = PRIMITIVES.try_lock() else { return };
        let Some(primitives) = primitives_slot.as_ref() else { return };
        crate::vm::liveeval::tick(ready.lua_state, primitives);
        presence.on_tick(ready.lua_state, primitives, play_test)
    };

    if keep {
        return;
    }
    let Ok(mut slot) = DISCOVERY.try_lock() else { return };
    if let Some(discovery) = slot.as_mut() {
        discovery.mark_placeless(ready.lua_state);
    }
}

fn drive_idle() {
    let Ok(mut presence_slot) = PRESENCE.try_lock() else { return };
    if let Some(presence) = presence_slot.as_mut() {
        presence.on_idle();
    }
}

pub fn install() -> Result<usize, InstallError> {
    let flags = luau_compile::enable_luau_flags();
    crate::log(&format!("luau: enabled {flags} compiler flag(s)"));

    let resolved = resolve::resolve().map_err(InstallError::Resolve)?;
    crate::log(&resolved.summarize());

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

    let probe = resolved.call_dispatch.and_then(LuaProbe::from_call_dispatch);
    match probe {
        Some(probe) => crate::log(&format!(
            "layout: lua_State global=+{:#x} top=+{:#x} (read from call_dispatch)",
            probe.global, probe.top
        )),
        None => crate::log("layout: lua_State offsets unreadable, presence disabled"),
    }
    if let Some(probe) = probe {
        *DISCOVERY.lock().unwrap_or_else(|poison| poison.into_inner()) =
            Some(Discovery::new(vtables, probe));
    }

    if let (Some(load), Some(call), Some(probe)) =
        (resolved.luau_load, resolved.call_dispatch, probe)
    {
        let caps = CapabilityLayout::derive(resolved.set_proto_caps, resolved.get_thread_caps);
        #[cfg(target_os = "windows")]
        let caps = caps.or_else(CapabilityLayout::windows_probe);
        match caps {
            Some(caps) => crate::log(&format!(
                "layout: caps proto_userdata=+{:#x} children=+{:#x} count=+{:#x} extra_caps=+{:#x}",
                caps.proto_userdata,
                caps.proto_children,
                caps.proto_child_count,
                caps.extra_capabilities
            )),
            None => crate::log("layout: capability offsets unreadable, elevation disabled"),
        }
        *PRIMITIVES.lock().unwrap_or_else(|p| p.into_inner()) = Some(Primitives {
            load,
            call,
            new_thread: resolved.lua_newthread,
            security_context_current: resolved.security_context_current,
            lua_top: probe.top,
            caps,
        });
        *PRESENCE.lock().unwrap_or_else(|p| p.into_inner()) = crate::discord::start();
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
