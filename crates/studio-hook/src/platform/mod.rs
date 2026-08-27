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

/// The Roblox Studio version string embedded in the main image.
///
/// Scans the read-only sections for a `0.<3>.<1-3>.<6-8>` shaped string, which
/// matches Studio's version without a fixed offset that breaks on updates.
///
/// # Examples
/// ```ignore
/// if let Some(v) = studio_version() { crate::log(&v); }
/// ```
pub fn studio_version() -> Option<String> {
    let image = find_main_image()?;
    let mut segments: Vec<Segment> = image.text_segments();
    segments.extend(image.data_segments());
    segments
        .into_iter()
        .filter_map(|segment: Segment| segment.as_slice())
        .find_map(scan_version)
}

fn scan_version(bytes: &[u8]) -> Option<String> {
    let mut i: usize = 0;
    while i + 12 <= bytes.len() {
        if bytes[i] == b'0' && bytes[i + 1] == b'.' {
            if let Some(version) = parse_version(&bytes[i..]) {
                return Some(version);
            }
        }
        i += 1;
    }
    None
}

fn parse_version(bytes: &[u8]) -> Option<String> {
    let mut pos: usize = 0;
    for (index, (min, max)) in [(1usize, 2usize), (3, 3), (1, 3), (6, 8)].into_iter().enumerate() {
        if index != 0 {
            if bytes.get(pos) != Some(&b'.') {
                return None;
            }
            pos += 1;
        }
        let start: usize = pos;
        while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
        if !(min..=max).contains(&(pos - start)) {
            return None;
        }
    }
    if bytes.get(pos).is_some_and(|b: &u8| b.is_ascii_digit() || *b == b'.') {
        return None;
    }
    core::str::from_utf8(&bytes[..pos]).ok().map(str::to_owned)
}
