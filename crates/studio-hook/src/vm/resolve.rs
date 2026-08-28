use crate::platform::{self, Segment};
use crate::scan::Pattern;
use crate::vm::signatures as sig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveError {
    NoImage,
    BadPattern(&'static str),
    NotFound(&'static str),
    Ambiguous(&'static str, usize),
}

#[derive(Debug, Clone)]
pub struct JobVtable {
    pub rtti: &'static str,
    pub vtable: usize,
    pub slot_index: usize,
    pub slot_addr: usize,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub slide: isize,
    pub step: usize,
    pub luau_load: Option<usize>,
    pub call_dispatch: Option<usize>,
    pub task_defer: Option<usize>,
    pub lua_newthread: Option<usize>,
    pub set_proto_caps: Option<usize>,
    pub get_thread_caps: Option<usize>,
    pub security_context_current: Option<usize>,
    pub data_model_vtable: Option<usize>,
    pub script_context_vtable: Option<usize>,
    pub waiting_hybrid_vtable: Option<usize>,
    pub jobs: Vec<JobVtable>,
}

fn scan_unique(
    name: &'static str,
    pattern: &Pattern,
    text: &[Segment],
) -> Result<usize, ResolveError> {
    let mut hits = Vec::new();
    for segment in text {
        let Some(bytes) = segment.as_slice() else { continue };
        hits.extend(pattern.find_all(bytes).into_iter().map(|at| segment.start + at));
        if hits.len() > 1 {
            break;
        }
    }
    match hits.len() {
        0 => Err(ResolveError::NotFound(name)),
        1 => Ok(hits[0]),
        n => Err(ResolveError::Ambiguous(name, n)),
    }
}

/// Bytes at `addr`, formatted as a signature string ready to paste into `signatures`.
fn signature_at(addr: usize, pattern: &Pattern, text: &[Segment]) -> Option<String> {
    let segment = text.iter().find(|segment| segment.contains(addr))?;
    let bytes = segment.as_slice()?;
    let start = addr.checked_sub(segment.start)?;
    let window = bytes.get(start..start.checked_add(pattern.len())?)?;
    Some(pattern.render(window))
}

fn find_unique(name: &'static str, spec: &str, text: &[Segment]) -> Result<usize, ResolveError> {
    let pattern = Pattern::parse(spec).map_err(|_| ResolveError::BadPattern(name))?;
    let strict = scan_unique(name, &pattern, text);
    if !matches!(strict, Err(ResolveError::NotFound(_))) {
        return strict;
    }
    let Some(relaxed) = pattern.relaxed() else { return strict };
    match scan_unique(name, &relaxed, text) {
        Ok(addr) => {
            let found = signature_at(addr, &pattern, text).unwrap_or_else(|| "?".to_owned());
            crate::log(&format!(
                "resolve: {name} drifted - matched with offsets masked at {addr:#x}, refresh to: {found}"
            ));
            Ok(addr)
        }
        Err(_) => strict,
    }
}

fn find_optional(name: &'static str, spec: &str, text: &[Segment]) -> Option<usize> {
    match find_unique(name, spec, text) {
        Ok(addr) => Some(addr),
        Err(err) => {
            crate::log(&format!("resolve: {name} unavailable ({err:?})"));
            None
        }
    }
}

fn find_first(name: &'static str, spec: &str, text: &[Segment]) -> Option<usize> {
    let pattern = Pattern::parse(spec).ok()?;
    for candidate in [Some(pattern.clone()), pattern.relaxed()].into_iter().flatten() {
        for segment in text {
            let Some(bytes) = segment.as_slice() else { continue };
            if let Some(at) = candidate.find_all(bytes).into_iter().next() {
                return Some(segment.start + at);
            }
        }
    }
    crate::log(&format!("resolve: {name} unavailable (NotFound)"));
    None
}

fn find_security_context_current(text: &[Segment]) -> Option<usize> {
    if sig::CAN_ACCESS_RESTRICTED.is_empty() {
        return None;
    }
    let checker = find_first("can_access_restricted", sig::CAN_ACCESS_RESTRICTED, text)?;
    let call_site = checker + sig::CAN_ACCESS_RESTRICTED_BL;
    #[cfg(target_arch = "aarch64")]
    {
        let instruction: u32 = crate::mem::read(call_site).ok()?;
        crate::scan::decode_arm64_bl(instruction, call_site)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let rel: i32 = crate::mem::read(call_site + 1).ok()?;
        Some((call_site as isize + 5 + rel as isize) as usize)
    }
}

pub fn resolve() -> Result<Resolved, ResolveError> {
    let image = platform::find_main_image().ok_or(ResolveError::NoImage)?;
    let text = image.text_segments();
    let data = image.data_segments();

    let step = find_unique("step", sig::STEP, &text)?;

    let data_model_vtable = platform::find_primary_vtable(sig::DATA_MODEL_RTTI, &text, &data);
    let script_context_vtable = platform::find_primary_vtable(sig::SCRIPT_CONTEXT_RTTI, &text, &data);
    let waiting_hybrid_vtable = platform::find_primary_vtable(sig::WAITING_HYBRID_RTTI, &text, &data);

    let mut jobs = Vec::new();
    for rtti in sig::JOB_CLASSES {
        let Some(vtable) = platform::find_primary_vtable(rtti, &text, &data) else { continue };
        let Some(slot_index) = platform::vtable_slot_of(vtable, step, 12) else { continue };
        jobs.push(JobVtable { rtti, vtable, slot_index, slot_addr: vtable + slot_index * 8 });
    }

    Ok(Resolved {
        slide: image.slide,
        step,
        luau_load: find_optional("luau_load", sig::LUAU_LOAD_WRAPPER, &text),
        call_dispatch: find_optional("call_dispatch", sig::CALL_DISPATCH, &text),
        task_defer: find_optional("task_defer", sig::TASK_DEFER, &text),
        lua_newthread: find_optional("lua_newthread", sig::LUA_NEWTHREAD, &text),
        set_proto_caps: find_optional("set_proto_caps", sig::SET_PROTO_CAPS, &text),
        get_thread_caps: find_optional("get_thread_caps", sig::GET_THREAD_CAPS, &text),
        security_context_current: find_security_context_current(&text),
        data_model_vtable,
        script_context_vtable,
        waiting_hybrid_vtable,
        jobs,
    })
}

impl Resolved {
    pub fn file_offset(&self, addr: usize) -> usize {
        (addr as isize - self.slide) as usize
    }

    pub fn summarize(&self) -> String {
        let mut out = format!(
            "step={:#x} load={:x?} call={:x?} defer={:x?} newthread={:x?}",
            self.file_offset(self.step),
            self.luau_load.map(|a| self.file_offset(a)),
            self.call_dispatch.map(|a| self.file_offset(a)),
            self.task_defer.map(|a| self.file_offset(a)),
            self.lua_newthread.map(|a| self.file_offset(a)),
        );
        out.push_str(&format!(
            "\nvtables datamodel={:x?} scriptcontext={:x?} hybrid={:x?}",
            self.data_model_vtable.map(|a| self.file_offset(a)),
            self.script_context_vtable.map(|a| self.file_offset(a)),
            self.waiting_hybrid_vtable.map(|a| self.file_offset(a)),
        ));
        for job in &self.jobs {
            out.push_str(&format!(
                "\njob {} slot={} at={:#x}",
                job.rtti,
                job.slot_index,
                self.file_offset(job.slot_addr)
            ));
        }
        out
    }
}
