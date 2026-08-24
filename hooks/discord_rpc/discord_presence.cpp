#include "discord_presence.h"
#include "discord_ipc.h"
#include "discord_place_lookup.h"
#include "luau_runtime.h"
#include "vm_platform.h"

#include <cctype>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <string>

static const char *DISCORD_RPC_CONFIG_PATH = "/Users/Shared/rbx-theme-set/DiscordRPC.json";

static bool readFile(const char *path, std::string &out) {
    FILE *f = fopen(path, "rb");
    if (!f) return false;
    fseek(f, 0, SEEK_END);
    long size = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (size <= 0) {
        fclose(f);
        return false;
    }
    out.resize((size_t)size);
    size_t n = fread(&out[0], 1, (size_t)size, f);
    fclose(f);
    out.resize(n);
    return true;
}

static bool jsonFindValueStart(const std::string &json, const char *key, size_t &valueStart) {
    std::string needle = std::string("\"") + key + "\"";
    size_t pos = json.find(needle);
    if (pos == std::string::npos) return false;
    pos = json.find(':', pos + needle.size());
    if (pos == std::string::npos) return false;
    pos++;
    while (pos < json.size() && isspace((unsigned char)json[pos])) pos++;
    valueStart = pos;
    return true;
}

static bool jsonExtractBool(const std::string &json, const char *key, bool &out) {
    size_t pos;
    if (!jsonFindValueStart(json, key, pos)) return false;
    if (json.compare(pos, 4, "true") == 0) {
        out = true;
        return true;
    }
    if (json.compare(pos, 5, "false") == 0) {
        out = false;
        return true;
    }
    return false;
}

// Tab-separated: placeId, active script's Name, active script's ClassName,
// cursor line, cursor character, isRunning ("true"/"false"). Everything
// past placeId is best-effort and left empty/"false" on any failure -
// StudioService/ScriptEditorService/RunService access are each wrapped in
// their own pcall so a transient failure in one can't take down PlaceId
// tracking, which has run standalone since before StudioService was
// reachable at all.
//
// Cursor position: mirrors roblox-modloader's edit.luau heuristic exactly
// (the last non-command-bar ScriptEditorService document, not a rigorous
// match against ActiveScript) rather than trying to correlate documents to
// the active script by reference - simpler, and this is what an existing,
// working reference implementation already established is good enough.
//
// isRunning: RunService:IsRunning() is false while merely editing and true
// once Play/Run actually starts simulating - checked on the SAME captured
// edit-mode lua_State this whole file already polls, not a second,
// separately-discovered play-test DataModel. Play/Run in Studio spins up
// its own DataModel(s) for the simulated server/client, but the original
// edit DataModel stays alive (just backgrounded) for the duration, and
// RunService's running state is a DataModel-global flag Studio sets
// regardless of which DataModel instance happens to be asking - confirmed
// live 2026-08-23, no separate discovery needed.
// Recompiled and run fresh via LuauRuntime_RunSync every poll tick, rather
// than cached once via LuauRuntime_LoadPersistent/CallPersistent - that
// pinned-closure path hung Studio deterministically on its own first
// invocation, every single test tonight (2026-08-23), even with content
// already proven safe through RunSync alone (the isolated StudioService/
// ScriptEditorService/RunService:IsRunning diagnostics all passed through
// RunSync in every run). RunSync recompiling this ~600-byte script every
// POLL_INTERVAL_MS is cheap, and each call already pops the Lua stack back
// down to the closure slot afterward (invokeAndDecode), so nothing
// accumulates the way the original unbounded-leak bug did.
static const char *POLL_SCRIPT = R"(
local placeId = tostring(game.PlaceId)
local scriptName, scriptClass = "", ""
local startLine, startCharacter = "", ""
local isRunning = "false"

local ok, active = pcall(function() return game:GetService("StudioService").ActiveScript end)
if ok and active then
    scriptName = active.Name
    scriptClass = active.ClassName

    local ok2, documents = pcall(function() return game:GetService("ScriptEditorService"):GetScriptDocuments() end)
    if ok2 and documents then
        for i = #documents, 1, -1 do
            local doc = documents[i]
            local ok3, isCmd = pcall(function() return doc:IsCommandBar() end)
            if ok3 and not isCmd then
                local ok4, line, char = pcall(function() return doc:GetSelectionStart() end)
                if ok4 then
                    startLine, startCharacter = tostring(line), tostring(char)
                end
                break
            end
        end
    end
end

local diag = ""
pcall(function()
    local rs = game:GetService("RunService")
    isRunning = tostring(rs:IsRunning())
    diag = tostring(rs:IsEdit()) .. "," .. tostring(rs:IsServer()) .. "," .. tostring(rs:IsClient())
end)

return placeId .. "\t" .. scriptName .. "\t" .. scriptClass .. "\t" .. startLine .. "\t" .. startCharacter .. "\t" .. isRunning .. "\t" .. diag
)";

#define POLL_INTERVAL_MS 1000

static bool gStarted = false;
static uint64_t gLastPollMs = 0;
static std::string gLastPlaceId;
static std::string gLastState, gLastDetails, gLastLargeImageUrl, gLastSmallImageKey, gLastJoinUrl;
static int64_t gSessionStartUnix = 0;

// This project's own uploaded Discord Rich Presence Art Assets (Discord
// Developer Portal, application 1540119100318027776) - established
// 2026-08-20 (script/localscript/modulescript) and 2026-08-23 (play/stop,
// sourced from the "Vanilla 3 for Roblox Studio" theme pack's own
// general/Play.png + general/Stop.png, upscaled to Discord's 512x512
// minimum via `sips -z 512 512` the same way the original three were).
// Class-keyed while editing, swapped to "play" while Play/Run is actually
// simulating (see isRunning in POLL_SCRIPT) - distinct from the
// generic "studio"/"play" scheme other Roblox Discord RPC integrations
// (e.g. roblox-modloader's discord_rpc example) use.
static const char *iconKeyForScriptClass(const std::string &className) {
    if (className == "Script") return "script";
    if (className == "LocalScript") return "localscript";
    if (className == "ModuleScript") return "modulescript";
    return "";
}

// Splits on '\t' into exactly `count` fields - fields beyond what's present
// stay empty rather than erroring, so a short/malformed result degrades
// gracefully instead of crashing the poll loop.
static void splitTabFields(const std::string &s, std::string *fields, size_t count) {
    size_t start = 0;
    for (size_t i = 0; i < count; i++) {
        size_t tab = s.find('\t', start);
        if (tab == std::string::npos) {
            fields[i] = s.substr(start);
            start = s.size();
        } else {
            fields[i] = s.substr(start, tab - start);
            start = tab + 1;
        }
    }
}

static uint64_t monotonicMillis(void) {
    return (uint64_t)std::chrono::duration_cast<std::chrono::milliseconds>(
        std::chrono::steady_clock::now().time_since_epoch()).count();
}

void DiscordPresence_Start(const char *clientId) {
    std::string configJson;
    bool enabled = true;
    if (readFile(DISCORD_RPC_CONFIG_PATH, configJson)) {
        jsonExtractBool(configJson, "enabled", enabled);
    }
    if (!enabled) return;

    gSessionStartUnix = (int64_t)std::chrono::duration_cast<std::chrono::seconds>(
        std::chrono::system_clock::now().time_since_epoch()).count();
    DiscordIpc_Start(clientId);
    gStarted = true;
}

static void *gLastPollLuaState = NULL;

void DiscordPresence_Tick(void) {
    if (!gStarted || !VM_IsReady()) return;
    uint64_t now = monotonicMillis();
    // A fresh capture (place switch) means gLastPollMs is stale relative to
    // this new lua_State - without this, the first real poll against the
    // new place would still wait out a full POLL_INTERVAL_MS on top of
    // hookedStep's own VM_TOUCH_GRACE_MS gate, stacking more visible delay
    // on an already place-switch-triggered reset.
    void *currentL = VM_GetLuaState();
    if (currentL != gLastPollLuaState) {
        gLastPollLuaState = currentL;
        gLastPollMs = 0;
    }
    if (now - gLastPollMs < POLL_INTERVAL_MS) return;
    gLastPollMs = now;

    std::string result;
    if (!LuauRuntime_RunSync(POLL_SCRIPT, "=DiscordPresencePoll", result)) return;

    std::string fields[6];
    splitTabFields(result, fields, 6);
    const std::string &placeId = fields[0];
    // POLL_SCRIPT always returns tostring(game.PlaceId) as the first field,
    // which is always plain digits - a non-digit placeId means
    // invokeAndDecode's result-decoding loop picked up extra, unrelated
    // stack slots (an intermittent readback race on L->top - see its own
    // MAX_DECODED_RESULTS comment) rather than the one real string this
    // script returns. Skip the tick and keep the last-known-good presence
    // rather than showing garbage like "Place <function>" in Discord.
    if (placeId.empty() || placeId.find_first_not_of("0123456789") != std::string::npos) return;
    const std::string &scriptName = fields[1];
    const std::string &scriptClass = fields[2];
    const std::string &startLine = fields[3];
    const std::string &startCharacter = fields[4];
    // RunService:IsRunning() queried through the edit DataModel's own
    // lua_State never sees Play testing (see VM_IsPlayTestActive's own
    // comment) - kept as a harmless OR rather than removed outright, in
    // case a future Studio build changes that.
    bool isRunning = fields[5] == "true" || VM_IsPlayTestActive();

    // A placeId of "0" briefly during otherwise-normal idling (not a real
    // place close) was seen live 2026-08-23 - most likely the same
    // documented captured-state revalidation race already debounced
    // elsewhere in this project, surfacing here as a single spurious empty
    // poll rather than a resetCapturedState. Require it to repeat before
    // actually flipping the shown presence to "Not in a place", the same
    // debounce-the-symptom approach REVALIDATE_FAILURE_THRESHOLD already
    // uses - a real place close stays "0" past this window regardless.
    #define PLACEID_ZERO_DEBOUNCE_TICKS 2
    static int gConsecutiveZeroPlaceId = 0;
    if (placeId == "0") {
        gConsecutiveZeroPlaceId++;
        if (gConsecutiveZeroPlaceId < PLACEID_ZERO_DEBOUNCE_TICKS) return;
    } else {
        gConsecutiveZeroPlaceId = 0;
    }
    gLastPlaceId = placeId;

    std::string details, state, largeImageKey, smallImageKey, smallImageText, joinUrl;
    if (placeId == "0") {
        details = "Not in a place";
        state = "In Studio";
        largeImageKey = "roblox_logo";
    } else {
        std::string name, thumbnailUrl;
        bool isPublic = false;
        bool resolved = DiscordPlaceLookup_Get(placeId, name, thumbnailUrl, isPublic);
        details = resolved ? name : ("Place " + placeId);
        largeImageKey = thumbnailUrl;
        // Only a place games.roblox.com actually returned public info for
        // gets a join button - a private/unpublished place fails that same
        // lookup (see DiscordPlaceLookup_Get's own comment), so there's
        // nothing extra to check here.
        if (resolved && isPublic) joinUrl = "https://www.roblox.com/games/" + placeId;

        // Priority: actually playtesting overrides everything else (what
        // script happens to be open doesn't matter once Play/Run starts
        // simulating) - then a specific open script - then just idling on
        // the place/workspace itself with nothing focused.
        if (isRunning) {
            state = "Testing";
            smallImageKey = "play";
            smallImageText = "Testing";
        } else if (!scriptName.empty()) {
            state = "Editing " + scriptName;
            if (!startLine.empty()) state += " - Line " + startLine + ":" + startCharacter;
            smallImageKey = iconKeyForScriptClass(scriptClass);
            if (!smallImageKey.empty()) smallImageText = scriptClass;
        } else {
            state = "Editing Workspace";
            smallImageKey = "stop";
            smallImageText = "Not testing";
        }
    }

    if (state == gLastState && details == gLastDetails &&
        largeImageKey == gLastLargeImageUrl && smallImageKey == gLastSmallImageKey &&
        joinUrl == gLastJoinUrl) return;
    gLastState = state;
    gLastDetails = details;
    gLastLargeImageUrl = largeImageKey;
    gLastSmallImageKey = smallImageKey;
    gLastJoinUrl = joinUrl;
    DiscordIpc_SetActivity(state.c_str(), details.c_str(), gSessionStartUnix,
                            largeImageKey.c_str(), details.c_str(),
                            smallImageKey.c_str(), smallImageText.c_str(),
                            joinUrl.empty() ? "" : "View Place", joinUrl.c_str());
}
