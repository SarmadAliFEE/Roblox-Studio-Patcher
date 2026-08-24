use std::sync::Mutex;
use std::time::{Duration, Instant};

const RESOLVE_TTL: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Default)]
pub struct PlaceInfo {
    pub thumbnail_url: String,
    pub is_public: bool,
}

struct Cache {
    universe_id: String,
    info: PlaceInfo,
    resolved_at: Instant,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
static IN_FLIGHT: Mutex<Option<String>> = Mutex::new(None);

pub fn get(universe_id: &str) -> Option<PlaceInfo> {
    if universe_id.is_empty() || universe_id == "0" {
        return None;
    }
    let mut cached = None;
    let mut stale = true;
    {
        let guard = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = guard.as_ref() {
            if entry.universe_id == universe_id {
                cached = Some(entry.info.clone());
                stale = entry.resolved_at.elapsed() >= RESOLVE_TTL;
            }
        }
    }
    if cached.is_none() || stale {
        spawn_fetch(universe_id.to_owned());
    }
    cached
}

fn spawn_fetch(universe_id: String) {
    {
        let mut in_flight = IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner());
        if in_flight.as_deref() == Some(universe_id.as_str()) {
            return;
        }
        *in_flight = Some(universe_id.clone());
    }
    std::thread::spawn(move || {
        let info = PlaceInfo {
            is_public: fetch_is_public(&universe_id).unwrap_or(false),
            thumbnail_url: fetch_thumbnail(&universe_id).unwrap_or_default(),
        };
        crate::log(&format!(
            "discord: resolved universe {universe_id} public={} thumbnail={}",
            info.is_public,
            if info.thumbnail_url.is_empty() { "(none)" } else { &info.thumbnail_url }
        ));
        *CACHE.lock().unwrap_or_else(|p| p.into_inner()) = Some(Cache {
            universe_id: universe_id.clone(),
            info,
            resolved_at: Instant::now(),
        });
        let mut in_flight = IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner());
        if in_flight.as_deref() == Some(universe_id.as_str()) {
            *in_flight = None;
        }
    });
}

fn fetch_is_public(universe_id: &str) -> Option<bool> {
    let url = format!("https://games.roblox.com/v1/games?universeIds={universe_id}");
    let json = get_json(&url)?;
    let entry = first_data_entry(&json)?;
    let restricted = entry.get("isContentRestricted").and_then(|v| v.as_bool()).unwrap_or(true);
    Some(!restricted)
}

fn fetch_thumbnail(universe_id: &str) -> Option<String> {
    let url = format!(
        "https://thumbnails.roblox.com/v1/games/icons?universeIds={universe_id}&returnPolicy=PlaceHolder&size=512x512&format=Png&isCircular=false"
    );
    let json = get_json(&url)?;
    let entry = first_data_entry(&json)?;
    entry.get("imageUrl").and_then(|v| v.as_str()).map(|s| s.to_owned())
}

fn get_json(url: &str) -> Option<serde_json::Value> {
    let body = ureq::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .get(url)
        .call()
        .ok()?
        .into_string()
        .ok()?;
    serde_json::from_str(&body).ok()
}

fn first_data_entry(root: &serde_json::Value) -> Option<&serde_json::Value> {
    root.get("data")?.as_array()?.first()
}
