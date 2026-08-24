use core::time::Duration;
use std::time::Instant;

use crate::vm::{self, Cursor};

pub const REVALIDATE_EVERY: Duration = Duration::from_millis(5000);
pub const STALE_AFTER: Duration = Duration::from_millis(1500);
pub const SETTLE_FOR: Duration = Duration::from_millis(2000);

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

#[derive(Debug, Clone, Copy)]
enum Stage {
    DataModel { cursor: Cursor },
    ScriptContext,
    LuaState { cursor: Cursor },
}

#[derive(Debug, Clone, Copy)]
enum Capture {
    Idle,
    Searching {
        job: usize,
        stage: Stage,
        data_model: Option<usize>,
        script_context: Option<usize>,
    },
    Ready { ready: Ready, since: Instant },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Idle,
    Searching,
    Became(Ready),
    Live(Ready),
    Settling,
    Dropped(&'static str),
}

pub struct Discovery {
    vtables: Vtables,
    state: Capture,
    alternate: Option<usize>,
    last_seen_captured: Option<Instant>,
    last_revalidate: Option<Instant>,
}

impl Discovery {
    pub fn new(vtables: Vtables) -> Discovery {
        Discovery {
            vtables,
            state: Capture::Idle,
            alternate: None,
            last_seen_captured: None,
            last_revalidate: None,
        }
    }

    pub fn ready(&self) -> Option<Ready> {
        match self.state {
            Capture::Ready { ready, .. } => Some(ready),
            _ => None,
        }
    }

    pub fn settled(&self, now: Instant) -> Option<Ready> {
        match self.state {
            Capture::Ready { ready, since } if now.duration_since(since) >= SETTLE_FOR => Some(ready),
            _ => None,
        }
    }

    fn captured_job(&self) -> Option<usize> {
        match self.state {
            Capture::Idle => None,
            Capture::Searching { job, .. } => Some(job),
            Capture::Ready { ready, .. } => Some(ready.job),
        }
    }

    fn drop_capture(&mut self, why: &'static str) -> Event {
        self.state = Capture::Idle;
        self.last_seen_captured = None;
        self.last_revalidate = None;
        Event::Dropped(why)
    }

    fn is_hybrid(&self, job: usize) -> bool {
        match self.vtables.waiting_hybrid {
            Some(vtable) => vm::object_matches_vtable(job, vtable),
            None => true,
        }
    }

    fn still_valid(&self, ready: &Ready) -> bool {
        vm::object_matches_vtable(ready.data_model, self.vtables.data_model)
            && vm::object_matches_vtable(ready.script_context, self.vtables.script_context)
    }

    pub fn on_step(&mut self, job: usize, now: Instant) -> Event {
        if self.captured_job() != Some(job) {
            return self.on_foreign_job(job, now);
        }
        self.last_seen_captured = Some(now);

        match self.state {
            Capture::Idle => Event::Idle,
            Capture::Searching { .. } => self.advance(now),
            Capture::Ready { ready, since } => {
                let due = self
                    .last_revalidate
                    .map(|at| now.duration_since(at) >= REVALIDATE_EVERY)
                    .unwrap_or(true);
                if due {
                    self.last_revalidate = Some(now);
                    if !self.still_valid(&ready) {
                        return self.drop_capture("captured objects no longer match their vtables");
                    }
                }
                if now.duration_since(since) < SETTLE_FOR {
                    Event::Settling
                } else {
                    Event::Live(ready)
                }
            }
        }
    }

    fn on_foreign_job(&mut self, job: usize, now: Instant) -> Event {
        self.alternate = Some(job);

        let stale = self
            .last_seen_captured
            .map(|at| now.duration_since(at) > STALE_AFTER)
            .unwrap_or(false);

        match self.state {
            Capture::Idle => {
                self.state = Capture::Searching {
                    job,
                    stage: Stage::DataModel { cursor: Cursor::default() },
                    data_model: None,
                    script_context: None,
                };
                self.last_seen_captured = Some(now);
                Event::Searching
            }
            _ if stale => self.drop_capture("captured job stopped stepping"),
            _ => Event::Idle,
        }
    }

    fn advance(&mut self, now: Instant) -> Event {
        let Capture::Searching { job, mut stage, mut data_model, mut script_context } = self.state else {
            return Event::Idle;
        };

        match stage {
            Stage::DataModel { mut cursor } => {
                match vm::find_instance_by_vtable(job, 0x40, self.vtables.data_model, &mut cursor) {
                    Some(found) => {
                        data_model = Some(found);
                        stage = Stage::ScriptContext;
                    }
                    None => stage = Stage::DataModel { cursor },
                }
            }
            Stage::ScriptContext => {
                if !self.is_hybrid(job) {
                    return self.drop_capture("job is not a script-hosting job");
                }
                match self.find_script_context(job) {
                    Some(found) => {
                        script_context = Some(found);
                        stage = Stage::LuaState { cursor: Cursor::default() };
                    }
                    None => return self.drop_capture("no ScriptContext reachable from job"),
                }
            }
            Stage::LuaState { mut cursor } => {
                let Some(context) = script_context else {
                    return self.drop_capture("lost ScriptContext mid-search");
                };
                match vm::find_lua_state_near(context, 0x100, &mut cursor) {
                    Some(lua_state) => {
                        let Some(data_model) = data_model else {
                            return self.drop_capture("lost DataModel mid-search");
                        };
                        let ready = Ready { job, data_model, script_context: context, lua_state };
                        self.state = Capture::Ready { ready, since: now };
                        self.last_revalidate = Some(now);
                        return Event::Became(ready);
                    }
                    None => stage = Stage::LuaState { cursor },
                }
            }
        }

        self.state = Capture::Searching { job, stage, data_model, script_context };
        Event::Searching
    }

    fn find_script_context(&self, job: usize) -> Option<usize> {
        (0..0x80).find_map(|index| {
            let addr = job + index * 8;
            let value = crate::mem::read_ptr(addr).ok()?;
            vm::object_matches_vtable(value, self.vtables.script_context).then_some(value)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Arena {
        words: Vec<u64>,
    }

    impl Arena {
        fn new(words: usize) -> Arena {
            Arena { words: vec![0u64; words] }
        }
        fn addr(&self, word: usize) -> usize {
            &self.words[word] as *const u64 as usize
        }
        fn put(&mut self, word: usize, value: usize) {
            self.words[word] = value as u64;
        }
    }

    fn vtables() -> Vtables {
        Vtables { data_model: 0xdd00_0000, script_context: 0xcc00_0000, waiting_hybrid: None }
    }

    #[test]
    fn first_job_seen_starts_a_search() {
        let mut discovery = Discovery::new(vtables());
        let now = Instant::now();
        assert_eq!(discovery.on_step(0x1234, now), Event::Searching);
        assert!(discovery.ready().is_none());
    }

    #[test]
    fn a_ready_capture_always_carries_every_pointer() {
        let mut arena = Arena::new(64);
        arena.put(0, vtables().data_model);
        let data_model = arena.addr(0);
        arena.put(8, vtables().script_context);
        let script_context = arena.addr(8);

        let ready = Ready { job: 1, data_model, script_context, lua_state: 2 };
        let discovery = Discovery {
            vtables: vtables(),
            state: Capture::Ready { ready, since: Instant::now() },
            alternate: None,
            last_seen_captured: None,
            last_revalidate: None,
        };
        let got = discovery.ready().expect("ready");
        assert_eq!(got.data_model, data_model);
        assert_eq!(got.script_context, script_context);
        assert_eq!(got.lua_state, 2);
    }

    #[test]
    fn capture_is_dropped_when_its_objects_stop_matching() {
        let mut arena = Arena::new(64);
        arena.put(0, 0xbad0_0000);
        let data_model = arena.addr(0);
        arena.put(8, 0xbad0_0000);
        let script_context = arena.addr(8);

        let ready = Ready { job: 0x99, data_model, script_context, lua_state: 3 };
        let start = Instant::now();
        let mut discovery = Discovery {
            vtables: vtables(),
            state: Capture::Ready { ready, since: start },
            alternate: None,
            last_seen_captured: Some(start),
            last_revalidate: None,
        };

        let event = discovery.on_step(0x99, start + REVALIDATE_EVERY);
        assert!(matches!(event, Event::Dropped(_)));
        assert!(discovery.ready().is_none());
    }

    #[test]
    fn a_ready_capture_is_not_reported_live_until_it_settles() {
        let ready = Ready { job: 7, data_model: 1, script_context: 2, lua_state: 3 };
        let start = Instant::now();
        let discovery = Discovery {
            vtables: vtables(),
            state: Capture::Ready { ready, since: start },
            alternate: None,
            last_seen_captured: Some(start),
            last_revalidate: Some(start),
        };
        assert!(discovery.settled(start).is_none());
        assert_eq!(discovery.settled(start + SETTLE_FOR), Some(ready));
    }

    #[test]
    fn a_silent_captured_job_is_dropped_when_another_job_steps() {
        let ready = Ready { job: 0x11, data_model: 1, script_context: 2, lua_state: 3 };
        let start = Instant::now();
        let mut discovery = Discovery {
            vtables: vtables(),
            state: Capture::Ready { ready, since: start },
            alternate: None,
            last_seen_captured: Some(start),
            last_revalidate: Some(start),
        };

        assert_eq!(discovery.on_step(0x22, start + Duration::from_millis(100)), Event::Idle);
        let event = discovery.on_step(0x22, start + STALE_AFTER + Duration::from_millis(1));
        assert!(matches!(event, Event::Dropped(_)));
    }
}
