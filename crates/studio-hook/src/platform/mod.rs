#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub start: usize,
    pub end: usize,
}

impl Segment {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, addr: usize) -> bool {
        (self.start..self.end).contains(&addr)
    }

    pub fn as_slice(&self) -> Option<&'static [u8]> {
        if self.is_empty() {
            return None;
        }
        Some(unsafe { core::slice::from_raw_parts(self.start as *const u8, self.len()) })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchError {
    Protect,
    Write,
}
