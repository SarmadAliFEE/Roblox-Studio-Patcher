use core::time::Duration;
use std::time::Instant;

use crate::vm::{self, Cursor, GAME_STATE_EDIT};

pub const STALE_AFTER: Duration = Duration::from_millis(700);
pub const PLAY_TEST_RECENT: Duration = Duration::from_millis(3000);
pub const FORGET_AFTER: Duration = Duration::from_millis(8000);
pub const MAX_TRACKED: usize = 24;
const DATA_MODEL_FIELDS: usize = 0x40;

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
    lua_state: Option<usize>,
    game_state: i32,
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
            lua_state: None,
            game_state: -1,
            last_seen: now,
        }
    }

    fn is_edit(&self) -> bool {
        self.game_state == GAME_STATE_EDIT
    }

    fn edit_ready(&self) -> Option<Ready> {
        Some(Ready {
            job: self.job,
            data_model: self.data_model?,
            script_context: self.script_context?,
            lua_state: self.lua_state?,
        })
    }
}

pub struct Discovery {
    vtables: Vtables,
    jobs: Vec<Tracked>,
    live_edit: Option<usize>,
    capture: Option<Ready>,
}

impl Discovery {
    pub fn new(vtables: Vtables) -> Discovery {
        Discovery { vtables, jobs: Vec::new(), live_edit: None, capture: None }
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
            .filter(|j| j.is_edit() && now.duration_since(j.last_seen) < STALE_AFTER)
            .filter_map(Tracked::edit_ready)
            .find(|ready| self.still_live(ready))
    }

    fn still_live(&self, ready: &Ready) -> bool {
        vm::object_matches_vtable(ready.data_model, self.vtables.data_model)
            && vm::object_matches_vtable(ready.script_context, self.vtables.script_context)
            && vm::is_edit_data_model(ready.data_model)
    }

    pub fn play_test_active(&self, now: Instant) -> bool {
        self.jobs.iter().any(|j| {
            vm::is_play_test_state(j.game_state) && now.duration_since(j.last_seen) < PLAY_TEST_RECENT
        })
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
                if let Some(state) = vm::game_state_type(data_model) {
                    self.jobs[index].game_state = state;
                }
            } else {
                let entry = &mut self.jobs[index];
                entry.data_model = None;
                entry.script_context = None;
                entry.lua_state = None;
                entry.game_state = -1;
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
                entry.game_state = vm::game_state_type(data_model).unwrap_or(-1);
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
        if !entry.is_edit() || entry.data_model.is_none() || entry.lua_state.is_some() {
            return;
        }
        let data_model = entry.data_model;
        if let Some((context, lua_state)) = self.jobs.iter().find_map(|j| {
            (j.data_model == data_model && j.lua_state.is_some())
                .then(|| (j.script_context, j.lua_state))
        }) {
            let entry = &mut self.jobs[index];
            entry.script_context = context;
            entry.lua_state = lua_state;
            return;
        }
        let context = self.find_script_context(job);
        let lua_state = context.and_then(|context| {
            vm::authoritative_lua_state(context)
                .or_else(|| vm::find_lua_state_near(context, 0x100, &mut Cursor::default()))
        });
        let Some(context) = context else { return };
        let entry = &mut self.jobs[index];
        entry.script_context = Some(context);
        entry.lua_state = lua_state;
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

    fn vtables() -> Vtables {
        Vtables { data_model: 0xdd00_0000, script_context: 0xcc00_0000, waiting_hybrid: None }
    }

    fn edit(lua_state: usize, last_seen: Instant) -> Tracked {
        Tracked {
            data_model: Some(0x1000),
            script_context: Some(0x2000),
            lua_state: Some(lua_state),
            game_state: GAME_STATE_EDIT,
            ..Tracked::new(1, last_seen)
        }
    }

    #[test]
    fn a_fresh_discovery_reports_nothing() {
        let discovery = Discovery::new(vtables());
        assert!(discovery.edit(Instant::now()).is_none());
        assert!(!discovery.play_test_active(Instant::now()));
    }

    #[test]
    fn a_recent_edit_job_is_reported_as_the_capture() {
        let now = Instant::now();
        // back the objects with memory so the live revalidation in edit() passes
        let mut dm = vec![0u64; 0x100];
        dm[0] = vtables().data_model as u64;
        let mut sc = vec![0u64; 4];
        sc[0] = vtables().script_context as u64;

        let mut discovery = Discovery::new(vtables());
        discovery.jobs.push(Tracked {
            data_model: Some(dm.as_ptr() as usize),
            script_context: Some(sc.as_ptr() as usize),
            lua_state: Some(0xabc),
            game_state: GAME_STATE_EDIT,
            ..Tracked::new(1, now)
        });
        assert_eq!(discovery.edit(now).map(|r| r.lua_state), Some(0xabc));
    }

    #[test]
    fn a_stale_edit_job_is_no_longer_the_capture() {
        let start = Instant::now();
        let mut discovery = Discovery::new(vtables());
        discovery.jobs.push(edit(0xabc, start));
        assert!(discovery.edit(start + STALE_AFTER + Duration::from_millis(1)).is_none());
    }

    #[test]
    fn an_edit_job_without_a_lua_state_is_not_ready() {
        let now = Instant::now();
        let mut discovery = Discovery::new(vtables());
        let mut pending = edit(0, now);
        pending.lua_state = None;
        discovery.jobs.push(pending);
        assert!(discovery.edit(now).is_none());
    }

    #[test]
    fn a_recent_play_datamodel_marks_play_test_active() {
        let now = Instant::now();
        let mut discovery = Discovery::new(vtables());
        discovery.jobs.push(Tracked {
            data_model: Some(0x3000),
            game_state: 1,
            ..Tracked::new(2, now)
        });
        assert!(discovery.play_test_active(now));
        assert!(!discovery.play_test_active(now + PLAY_TEST_RECENT + Duration::from_millis(1)));
    }

    #[test]
    fn eviction_keeps_the_table_bounded() {
        let now = Instant::now();
        let mut discovery = Discovery::new(vtables());
        for i in 0..MAX_TRACKED + 8 {
            discovery.slot_for(0x100 + i, now + Duration::from_millis(i as u64));
        }
        assert!(discovery.jobs.len() <= MAX_TRACKED);
    }
}
