pub mod ipc;
pub mod place;
pub mod presence;

use ipc::Ipc;
use presence::Presence;

const CLIENT_ID: &str = "1540119100318027776";

#[cfg(target_os = "macos")]
const CONFIG_PATH: &str = "/Users/Shared/rbx-theme-set/DiscordRPC.json";
#[cfg(target_os = "windows")]
const CONFIG_PATH: &str = r"C:\Users\Public\rbxthemeset\DiscordRPC.json";

pub fn start() -> Option<Presence> {
    if !enabled() {
        crate::log("discord: disabled by config");
        return None;
    }
    crate::log("discord: starting presence");
    Some(Presence::new(Ipc::start(CLIENT_ID.to_owned())))
}

fn enabled() -> bool {
    let Ok(text) = std::fs::read_to_string(CONFIG_PATH) else { return true };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|json| json.get("enabled").and_then(|v| v.as_bool()))
        .unwrap_or(true)
}
