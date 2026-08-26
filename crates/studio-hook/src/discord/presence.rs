use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::discord::ipc::{Activity, Ipc};
use crate::discord::place;
use crate::vm::exec::{self, Primitives, Value};

const POLL_INTERVAL: Duration = Duration::from_millis(400);
const ZERO_PLACE_DEBOUNCE: u32 = 2;

const POLL_SCRIPT: &str = r#"
local place = tostring(game.PlaceId)
local universe = tostring(game.GameId)

local name = game.Name
pcall(function()
    if __rpcNameFor ~= place then
        __rpcNameFor = place
        __rpcName = nil
        task.spawn(function()
            local ok, info = pcall(function()
                return game:GetService("MarketplaceService"):GetProductInfoAsync(game.PlaceId)
            end)
            if ok and info and info.Name then __rpcName = info.Name end
        end)
    end
    if __rpcName then name = __rpcName end
end)

local scriptName, scriptClass, line, char = "", "", "", ""

local ok, active = pcall(function() return game:GetService("StudioService").ActiveScript end)
if ok and active then
    scriptName, scriptClass = active.Name, active.ClassName
    local ok2, docs = pcall(function() return game:GetService("ScriptEditorService"):GetScriptDocuments() end)
    if ok2 and docs then
        for i = 1, #docs do
            local doc = docs[i]
            local okc, isCmd = pcall(function() return doc:IsCommandBar() end)
            if okc and not isCmd then
                local oks, docScript = pcall(function() return doc:GetScript() end)
                if oks and docScript == active then
                    local okl, l, c = pcall(function() return doc:GetSelectionStart() end)
                    if okl and l then line, char = tostring(l), tostring(c) end
                    break
                end
            end
        end
    end
end

local running = "false"
pcall(function() running = tostring(game:GetService("RunService"):IsRunning()) end)

return place .. "\t" .. universe .. "\t" .. name .. "\t" .. scriptName .. "\t" .. scriptClass .. "\t" .. line .. "\t" .. char .. "\t" .. running
"#;

struct Poll {
    place_id: String,
    universe_id: String,
    place_name: String,
    script_name: String,
    script_class: String,
    line: String,
    character: String,
    running: bool,
}

pub struct Presence {
    ipc: Ipc,
    session_start: i64,
    last_activity: Option<Activity>,
    last_poll: Option<Instant>,
    last_lua_state: usize,
    zero_place_streak: u32,
}

impl Presence {
    pub fn new(ipc: Ipc) -> Presence {
        let session_start = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Presence {
            ipc,
            session_start,
            last_activity: None,
            last_poll: None,
            last_lua_state: 0,
            zero_place_streak: 0,
        }
    }

    pub fn on_tick(&mut self, lua_state: usize, primitives: &Primitives, play_test: bool) {
        if lua_state != self.last_lua_state {
            self.last_lua_state = lua_state;
            self.last_poll = None;
            self.zero_place_streak = 0;
        }
        let now = Instant::now();
        if let Some(at) = self.last_poll {
            if now.duration_since(at) < POLL_INTERVAL {
                return;
            }
        }
        self.last_poll = Some(now);

        let Some(poll) = self.poll(lua_state, primitives) else { return };
        if let Some(activity) = self.build_activity(&poll, play_test) {
            self.push(activity);
        }
    }

    pub fn on_idle(&mut self) {
        let now = Instant::now();
        if let Some(at) = self.last_poll {
            if now.duration_since(at) < POLL_INTERVAL {
                return;
            }
        }
        self.last_poll = Some(now);
        self.last_lua_state = 0;
        self.zero_place_streak = 0;
        self.push(Activity {
            state: "In Studio".into(),
            details: "Not in a place".into(),
            large_image: "roblox_logo".into(),
            start_unix: self.session_start,
            ..Default::default()
        });
    }

    fn push(&mut self, activity: Activity) {
        if self.last_activity.as_ref() == Some(&activity) {
            return;
        }
        crate::log(&format!(
            "discord: presence details=\"{}\" state=\"{}\"",
            activity.details, activity.state
        ));
        self.last_activity = Some(activity.clone());
        self.ipc.set_activity(activity);
    }

    fn poll(&self, lua_state: usize, primitives: &Primitives) -> Option<Poll> {
        let values = match exec::run(lua_state, primitives, POLL_SCRIPT, "=DiscordPresencePoll") {
            Ok(values) => values,
            Err(err) => {
                crate::log(&format!("discord: poll failed: {err:?}"));
                return None;
            }
        };
        let Some(Value::Str(row)) = values.into_iter().next() else { return None };
        let mut fields = row.split('\t');
        Some(Poll {
            place_id: fields.next().unwrap_or_default().to_owned(),
            universe_id: fields.next().unwrap_or_default().to_owned(),
            place_name: fields.next().unwrap_or_default().to_owned(),
            script_name: fields.next().unwrap_or_default().to_owned(),
            script_class: fields.next().unwrap_or_default().to_owned(),
            line: fields.next().unwrap_or_default().to_owned(),
            character: fields.next().unwrap_or_default().to_owned(),
            running: fields.next() == Some("true"),
        })
    }

    fn build_activity(&mut self, poll: &Poll, play_test: bool) -> Option<Activity> {
        let running = poll.running || play_test;
        if poll.place_id.is_empty() || !poll.place_id.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }

        if poll.place_id == "0" {
            self.zero_place_streak += 1;
            if self.zero_place_streak < ZERO_PLACE_DEBOUNCE {
                return None;
            }
            return Some(Activity {
                state: "In Studio".into(),
                details: "Not in a place".into(),
                large_image: "roblox_logo".into(),
                start_unix: self.session_start,
                ..Default::default()
            });
        }
        self.zero_place_streak = 0;

        let resolved = place::get(&poll.universe_id);
        let details = if poll.place_name.is_empty() {
            format!("Place {}", poll.place_id)
        } else {
            poll.place_name.clone()
        };

        let mut activity = Activity {
            details: details.clone(),
            large_image: resolved.as_ref().map(|r| r.thumbnail_url.clone()).unwrap_or_default(),
            large_text: details,
            start_unix: self.session_start,
            ..Default::default()
        };

        if resolved.map(|r| r.is_public).unwrap_or(false) {
            activity.button_label = "View Place".into();
            activity.button_url = format!("https://www.roblox.com/games/{}", poll.place_id);
        }

        if running {
            activity.state = "Testing".into();
            activity.small_image = "play".into();
            activity.small_text = "Testing".into();
        } else if !poll.script_name.is_empty() {
            activity.state = format!("Editing {}", poll.script_name);
            if !poll.line.is_empty() {
                activity.state.push_str(&format!(" - Line {}:{}", poll.line, poll.character));
            }
            activity.small_image = icon_key(&poll.script_class).into();
            if !activity.small_image.is_empty() {
                activity.small_text = poll.script_class.clone();
            }
        } else {
            activity.state = "Editing Workspace".into();
            activity.small_image = "stop".into();
            activity.small_text = "Not testing".into();
        }

        Some(activity)
    }
}

fn icon_key(class: &str) -> &'static str {
    match class {
        "Script" => "script",
        "LocalScript" => "localscript",
        "ModuleScript" => "modulescript",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presence() -> Presence {
        Presence {
            ipc: Ipc::start(String::new()),
            session_start: 100,
            last_activity: None,
            last_poll: None,
            last_lua_state: 0,
            zero_place_streak: ZERO_PLACE_DEBOUNCE,
        }
    }

    fn poll(place: &str, name: &str, script: &str, running: bool) -> Poll {
        Poll {
            place_id: place.into(),
            universe_id: "0".into(),
            place_name: name.into(),
            script_name: script.into(),
            script_class: "Script".into(),
            line: "12".into(),
            character: "3".into(),
            running,
        }
    }

    #[test]
    fn a_non_numeric_place_id_is_rejected() {
        assert!(presence().build_activity(&poll("abc", "", "", false), false).is_none());
    }

    #[test]
    fn place_zero_reads_as_in_studio() {
        let activity = presence().build_activity(&poll("0", "", "", false), false).expect("activity");
        assert_eq!(activity.state, "In Studio");
        assert_eq!(activity.details, "Not in a place");
    }

    #[test]
    fn editing_a_script_shows_its_name_and_cursor() {
        let activity = presence().build_activity(&poll("55", "My Place", "Main", false), false).expect("activity");
        assert_eq!(activity.details, "My Place");
        assert_eq!(activity.state, "Editing Main - Line 12:3");
        assert_eq!(activity.small_image, "script");
    }

    #[test]
    fn a_running_place_reads_as_testing() {
        let activity = presence().build_activity(&poll("55", "My Place", "Main", true), false).expect("activity");
        assert_eq!(activity.state, "Testing");
        assert_eq!(activity.small_image, "play");
    }

    #[test]
    fn an_unnamed_place_falls_back_to_its_id() {
        let activity = presence().build_activity(&poll("77", "", "", false), false).expect("activity");
        assert_eq!(activity.details, "Place 77");
        assert_eq!(activity.state, "Editing Workspace");
    }
}
