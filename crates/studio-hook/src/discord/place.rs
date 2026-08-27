use std::sync::Mutex;
use std::time::{Duration, Instant};

const RESOLVE_TTL: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Default)]
pub struct PlaceInfo {
    pub thumbnail_url: String,
    pub is_public: bool,
    pub name: String,
}

struct Cache {
    key: String,
    info: PlaceInfo,
    resolved_at: Instant,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
static IN_FLIGHT: Mutex<Option<String>> = Mutex::new(None);

pub fn get(universe_id: &str, place_id: &str) -> Option<PlaceInfo> {
    if universe_id.is_empty() || universe_id == "0" {
        return None;
    }
    let key = format!("{universe_id}/{place_id}");
    let mut cached = None;
    let mut stale = true;
    {
        let guard = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = guard.as_ref() {
            if entry.key == key {
                cached = Some(entry.info.clone());
                stale = entry.resolved_at.elapsed() >= RESOLVE_TTL;
            }
        }
    }
    if cached.is_none() || stale {
        spawn_fetch(universe_id.to_owned(), place_id.to_owned(), key);
    }
    cached
}

fn spawn_fetch(universe_id: String, place_id: String, key: String) {
    {
        let mut in_flight = IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner());
        if in_flight.as_deref() == Some(key.as_str()) {
            return;
        }
        *in_flight = Some(key.clone());
    }
    std::thread::spawn(move || {
        let (is_public, universe_name) = fetch_details(&universe_id).unwrap_or((false, String::new()));
        let name = fetch_place_name(&place_id).unwrap_or(universe_name);
        let info = PlaceInfo {
            is_public,
            name,
            thumbnail_url: fetch_thumbnail(&universe_id).unwrap_or_default(),
        };
        crate::log(&format!(
            "discord: resolved universe {universe_id} public={} thumbnail={}",
            info.is_public,
            if info.thumbnail_url.is_empty() { "(none)" } else { &info.thumbnail_url }
        ));
        *CACHE.lock().unwrap_or_else(|p| p.into_inner()) = Some(Cache {
            key: key.clone(),
            info,
            resolved_at: Instant::now(),
        });
        let mut in_flight = IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner());
        if in_flight.as_deref() == Some(universe_id.as_str()) {
            *in_flight = None;
        }
    });
}

fn fetch_details(universe_id: &str) -> Option<(bool, String)> {
    let url = format!("https://games.roblox.com/v1/games?universeIds={universe_id}");
    let json = get_json(&url)?;
    let entry = first_data_entry(&json)?;
    let restricted = entry.get("isContentRestricted").and_then(|v| v.as_bool()).unwrap_or(true);
    let name = entry
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned();
    Some((!restricted, name))
}

fn fetch_place_name(place_id: &str) -> Option<String> {
    if place_id.is_empty() || place_id == "0" {
        return None;
    }
    let url = format!("https://economy.roblox.com/v2/assets/{place_id}/details");
    let json = get_json(&url)?;
    let name = json.get("Name").and_then(|v| v.as_str())?.to_owned();
    (!name.is_empty()).then_some(name)
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
