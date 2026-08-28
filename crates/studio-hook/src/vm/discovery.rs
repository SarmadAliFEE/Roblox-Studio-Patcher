use core::time::Duration;
use std::time::Instant;

use crate::vm::layout::LuaProbe;
use crate::vm::{self, Cursor};

pub const STALE_AFTER: Duration = Duration::from_millis(700);
pub const PLAY_TEST_RECENT: Duration = Duration::from_millis(3000);
pub const FORGET_AFTER: Duration = Duration::from_millis(8000);
pub const MAX_TRACKED: usize = 24;
const DATA_MODEL_FIELDS: usize = 0x40;
const PLACELESS_STRIKES: u32 = 3;
pub const PLACELESS_COOLDOWN: Duration = Duration::from_millis(15000);
const PROBE_INTERVAL: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vtables {
    pub data_model: usize,
    pub script_context: usize,
    pub waiting_hybrid: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ready {
    pub job: usize,
    pub data_model: usize,
    pub script_context: usize,
    pub lua_state: usize,
}

struct Tracked {
    job: usize,
    data_model: Option<usize>,
    dm_cursor: Cursor,
    dm_searched: bool,
    script_context: Option<usize>,
    lua_states: Vec<usize>,
    lua_index: usize,
    strikes: u32,
    retry_after: Option<Instant>,
    last_seen: Instant,
}

impl Tracked {
    fn new(job: usize, now: Instant) -> Tracked {
        Tracked {
            job,
            data_model: None,
            dm_cursor: Cursor::default(),
            dm_searched: false,
            script_context: None,
            lua_states: Vec::new(),
            lua_index: 0,
            strikes: 0,
            retry_after: None,
            last_seen: now,
        }
    }

    fn pollable(&self, now: Instant) -> bool {
        !self.lua_states.is_empty() && self.retry_after.map(|at| now >= at).unwrap_or(true)
    }

    fn current_lua_state(&self) -> Option<usize> {
        self.lua_states.get(self.lua_index).copied()
    }

    fn edit_ready(&self) -> Option<Ready> {
        Some(Ready {
            job: self.job,
            data_model: self.data_model?,
            script_context: self.script_context?,
            lua_state: self.current_lua_state()?,
        })
    }
}

pub struct Discovery {
    vtables: Vtables,
    probe: LuaProbe,
    jobs: Vec<Tracked>,
    live_edit: Option<usize>,
    capture: Option<Ready>,
    probe_cursor: usize,
    last_probe: Option<Instant>,
    running_until: Option<Instant>,
}

impl Discovery {
    pub fn new(vtables: Vtables, probe: LuaProbe) -> Discovery {
        Discovery {
            vtables,
            probe,
            jobs: Vec::new(),
            live_edit: None,
            capture: None,
            probe_cursor: 0,
            last_probe: None,
            running_until: None,
        }
    }

    /// Hands back a VM belonging to a different DataModel than `capture`, rotating through
    /// the tracked jobs. Studio runs a play session in its own DataModel, so whether a test
    /// is running can only be seen from a VM other than the edit one being polled.
    pub fn next_running_probe(&mut self, capture: &Ready, now: Instant) -> Option<usize> {
        if self.last_probe.map(|at| now.duration_since(at) < PROBE_INTERVAL).unwrap_or(false) {
            return None;
        }
        let candidates: Vec<usize> = self
            .jobs
            .iter()
            .filter(|job| job.data_model != Some(capture.data_model))
            .filter(|job| now.duration_since(job.last_seen) < STALE_AFTER)
            .filter_map(Tracked::current_lua_state)
            .filter(|state| self.probe.looks_like_lua_state(*state))
            .collect();
        if candidates.is_empty() {
            return None;
        }
        self.last_probe = Some(now);
        self.probe_cursor = self.probe_cursor.wrapping_add(1) % candidates.len();
        Some(candidates[self.probe_cursor])
    }

    pub fn note_running(&mut self, running: bool, now: Instant) {
        if running {
            self.running_until = Some(now + PLAY_TEST_RECENT);
        }
    }

    /// Records that `lua_state` polled without a place, so a Studio session holding
    /// several VMs eventually settles on the one with the open place.
    pub fn mark_placeless(&mut self, lua_state: usize) {
        let now = Instant::now();
        for job in self.jobs.iter_mut().filter(|j| j.current_lua_state() == Some(lua_state)) {
            job.strikes = job.strikes.saturating_add(1);
            if job.strikes < PLACELESS_STRIKES {
                continue;
            }
            job.strikes = 0;
            job.lua_index += 1;
            if job.lua_index >= job.lua_states.len() {
                job.lua_index = 0;
                job.retry_after = Some(now + PLACELESS_COOLDOWN);
            }
        }
        if self.capture.map(|ready| ready.lua_state) == Some(lua_state) {
            self.capture = None;
        }
    }

    pub fn on_step(&mut self, job: usize, now: Instant) {
        let index = self.slot_for(job, now);
        self.jobs[index].last_seen = now;
        self.locate_data_model(index, job);
        self.resolve_lua_state(index, job);
        self.retire_stale(now);
        self.capture = self.edit(now);
        self.announce_edit(now);
    }

    pub fn edit(&self, now: Instant) -> Option<Ready> {
        if let Some(ready) = self.capture {
            if self.still_live(&ready) {
                return Some(ready);
            }
        }
        self.jobs
            .iter()
            .filter(|j| j.pollable(now) && now.duration_since(j.last_seen) < STALE_AFTER)
            .filter_map(Tracked::edit_ready)
            .find(|ready| self.still_live(ready))
    }

    fn still_live(&self, ready: &Ready) -> bool {
        vm::object_matches_vtable(ready.data_model, self.vtables.data_model)
            && vm::object_matches_vtable(ready.script_context, self.vtables.script_context)
            && self.probe.looks_like_lua_state(ready.lua_state)
    }

    pub fn play_test_active(&self, now: Instant) -> bool {
        self.running_until.map(|until| now < until).unwrap_or(false)
    }

    fn slot_for(&mut self, job: usize, now: Instant) -> usize {
        if let Some(index) = self.jobs.iter().position(|j| j.job == job) {
            return index;
        }
        let fresh = Tracked::new(job, now);
        if self.jobs.len() < MAX_TRACKED {
            self.jobs.push(fresh);
            return self.jobs.len() - 1;
        }
        let evict = self
            .jobs
            .iter()
            .enumerate()
            .min_by_key(|(_, j)| j.last_seen)
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.jobs[evict] = fresh;
        evict
    }

    fn locate_data_model(&mut self, index: usize, job: usize) {
        if let Some(data_model) = self.jobs[index].data_model {
            if vm::object_matches_vtable(data_model, self.vtables.data_model) {
            } else {
                let entry = &mut self.jobs[index];
                entry.data_model = None;
                entry.script_context = None;
                entry.lua_states.clear();
                entry.lua_index = 0;
                entry.strikes = 0;
                entry.retry_after = None;
                entry.dm_searched = false;
                entry.dm_cursor.reset();
            }
            return;
        }
        if self.jobs[index].dm_searched {
            return;
        }

        let vtable = self.vtables.data_model;
        let found = vm::find_instance_by_vtable(job, DATA_MODEL_FIELDS, vtable, &mut self.jobs[index].dm_cursor);
        let entry = &mut self.jobs[index];
        match found {
            Some(data_model) => {
                entry.data_model = Some(data_model);
                entry.dm_searched = true;
            }
            None if entry.dm_cursor.exhausted() => {
                entry.dm_searched = true;
            }
            None => {}
        }
    }

    fn resolve_lua_state(&mut self, index: usize, job: usize) {
        let entry = &self.jobs[index];
        if entry.data_model.is_none() || !entry.lua_states.is_empty() {
            return;
        }
        let data_model = entry.data_model;
        if let Some((context, states)) = self.jobs.iter().find_map(|j| {
            (j.data_model == data_model && !j.lua_states.is_empty())
                .then(|| (j.script_context, j.lua_states.clone()))
        }) {
            let entry = &mut self.jobs[index];
            entry.script_context = context;
            entry.lua_states = states;
            entry.lua_index = 0;
            return;
        }
        let Some(context) = self.find_script_context(job) else { return };
        let states = vm::main_thread_candidates(context, &self.probe);
        if !states.is_empty() {
            crate::log(&format!(
                "vm: script context {context:#x} exposes {} main thread(s)",
                states.len()
            ));
        }
        if let Some(first) = states.first() {
            crate::vm::exec::calibrate_extra_space(*first, context);
        }
        let entry = &mut self.jobs[index];
        entry.script_context = Some(context);
        entry.lua_states = states;
        entry.lua_index = 0;
    }

    fn find_script_context(&self, job: usize) -> Option<usize> {
        (0..0x80).find_map(|index| {
            let value = crate::mem::read_ptr(job + index * 8).ok()?;
            vm::object_matches_vtable(value, self.vtables.script_context).then_some(value)
        })
    }

    fn retire_stale(&mut self, now: Instant) {
        self.jobs.retain(|j| now.duration_since(j.last_seen) < FORGET_AFTER);
    }

    fn announce_edit(&mut self, now: Instant) {
        let current = self.edit(now).map(|ready| ready.lua_state);
        if current != self.live_edit {
            match current {
                Some(lua_state) => crate::log(&format!("capture ready: edit lua_state={lua_state:#x}")),
                None => crate::log("capture dropped: no edit datamodel"),
            }
            self.live_edit = current;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe() -> LuaProbe {
        LuaProbe { global: 0x28, top: 0x18 }
    }

    fn vtables() -> Vtables {
        Vtables { data_model: 0xdd00_0000, script_context: 0xcc00_0000, waiting_hybrid: None }
    }

    fn backed_lua_state() -> usize {
        let global: &'static mut [u64] = Box::leak(vec![0u64; 4].into_boxed_slice());
        let state: &'static mut [u64] = Box::leak(vec![0u64; 16].into_boxed_slice());
        state[0] = 0x0a;
        state[0x28 / 8] = global.as_ptr() as u64;
        state.as_ptr() as usize
    }

    fn backed_edit(last_seen: Instant) -> Tracked {
        let dm: &'static mut [u64] = Box::leak(vec![0u64; 0x100].into_boxed_slice());
        dm[0] = vtables().data_model as u64;
        let sc: &'static mut [u64] = Box::leak(vec![0u64; 4].into_boxed_slice());
        sc[0] = vtables().script_context as u64;
        Tracked {
            data_model: Some(dm.as_ptr() as usize),
            script_context: Some(sc.as_ptr() as usize),
            lua_states: vec![backed_lua_state()],
            strikes: 0,
            ..Tracked::new(1, last_seen)
        }
    }

    fn edit(lua_state: usize, last_seen: Instant) -> Tracked {
        Tracked {
            data_model: Some(0x1000),
            script_context: Some(0x2000),
            lua_states: vec![lua_state],
            strikes: 0,
            ..Tracked::new(1, last_seen)
        }
    }

    #[test]
    fn a_fresh_discovery_reports_nothing() {
        let discovery = Discovery::new(vtables(), probe());
        assert!(discovery.edit(Instant::now()).is_none());
        assert!(!discovery.play_test_active(Instant::now()));
    }

    #[test]
    fn a_running_report_expires_once_the_test_stops() {
        let now = Instant::now();
        let mut discovery = Discovery::new(vtables(), probe());

        discovery.note_running(true, now);
        assert!(discovery.play_test_active(now));
        assert!(!discovery.play_test_active(now + PLAY_TEST_RECENT));

        discovery.note_running(false, now + PLAY_TEST_RECENT);
        assert!(!discovery.play_test_active(now + PLAY_TEST_RECENT));
    }

    #[test]
    fn a_recent_edit_job_is_reported_as_the_capture() {
        let now = Instant::now();
        // back the objects with memory so the live revalidation in edit() passes
        let mut dm = vec![0u64; 0x100];
        dm[0] = vtables().data_model as u64;
        let mut sc = vec![0u64; 4];
        sc[0] = vtables().script_context as u64;

        let lua_state = backed_lua_state();
        let mut discovery = Discovery::new(vtables(), probe());
        discovery.jobs.push(Tracked {
            data_model: Some(dm.as_ptr() as usize),
            script_context: Some(sc.as_ptr() as usize),
            lua_states: vec![lua_state],
            strikes: 0,
            ..Tracked::new(1, now)
        });
        assert_eq!(discovery.edit(now).map(|r| r.lua_state), Some(lua_state));
    }

    #[test]
    fn a_stale_edit_job_is_no_longer_the_capture() {
        let start = Instant::now();
        let mut discovery = Discovery::new(vtables(), probe());
        discovery.jobs.push(edit(0xabc, start));
        assert!(discovery.edit(start + STALE_AFTER + Duration::from_millis(1)).is_none());
    }

    #[test]
    fn an_edit_job_without_a_lua_state_is_not_ready() {
        let now = Instant::now();
        let mut discovery = Discovery::new(vtables(), probe());
        let mut pending = edit(0, now);
        pending.lua_states.clear();
        discovery.jobs.push(pending);
        assert!(discovery.edit(now).is_none());
    }

    #[test]
    fn a_placeless_vm_hands_over_to_the_next_one_in_the_context() {
        let now = Instant::now();
        let mut discovery = Discovery::new(vtables(), probe());
        let mut job = backed_edit(now);
        let second = backed_lua_state();
        job.lua_states.push(second);
        let first = job.lua_states[0];
        discovery.jobs.push(job);

        assert_eq!(discovery.edit(now).map(|r| r.lua_state), Some(first));
        for _ in 0..PLACELESS_STRIKES {
            discovery.mark_placeless(first);
        }
        assert_eq!(discovery.edit(now).map(|r| r.lua_state), Some(second));
    }

    #[test]
    fn a_vm_that_never_reports_a_place_is_given_up_on() {
        let now = Instant::now();
        let mut discovery = Discovery::new(vtables(), probe());
        discovery.jobs.push(backed_edit(now));
        let ready = discovery.edit(now).expect("a candidate to start with");

        for _ in 0..PLACELESS_STRIKES {
            discovery.mark_placeless(ready.lua_state);
        }
        assert!(discovery.edit(Instant::now()).is_none());
    }

    #[test]
    fn a_vm_is_kept_while_it_still_has_strikes_left() {
        let now = Instant::now();
        let mut discovery = Discovery::new(vtables(), probe());
        discovery.jobs.push(backed_edit(now));
        let ready = discovery.edit(now).expect("a candidate to start with");

        discovery.mark_placeless(ready.lua_state);
        assert!(discovery.edit(now).is_some());
    }

    #[test]
    fn eviction_keeps_the_table_bounded() {
        let now = Instant::now();
        let mut discovery = Discovery::new(vtables(), probe());
        for i in 0..MAX_TRACKED + 8 {
            discovery.slot_for(0x100 + i, now + Duration::from_millis(i as u64));
        }
        assert!(discovery.jobs.len() <= MAX_TRACKED);
    }
}
