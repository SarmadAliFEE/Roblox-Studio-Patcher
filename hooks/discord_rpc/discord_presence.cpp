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

static const char *POLL_SETUP_SCRIPT = R"(
__vmhook_pollFn = function()
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
            for i = 1, #documents do
                local doc = documents[i]
                local ok3, isCmd = pcall(function() return doc:IsCommandBar() end)
                if ok3 and not isCmd then
                    local ok5, docScript = pcall(function() return doc:GetScript() end)
                    if ok5 and docScript == active then
                        local ok4, line, char = pcall(function() return doc:GetSelectionStart() end)
                        if ok4 and line then
                            startLine, startCharacter = tostring(line), tostring(char)
                        end
                        break
                    end
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
end
return __vmhook_pollFn()
)";

#define POLL_INTERVAL_MS 1000

static bool gStarted = false;
static uint64_t gLastPollMs = 0;
static std::string gLastPlaceId;
static std::string gLastState, gLastDetails, gLastLargeImageUrl, gLastSmallImageKey, gLastJoinUrl;
static int64_t gSessionStartUnix = 0;

static const char *iconKeyForScriptClass(const std::string &className) {
    if (className == "Script") return "script";
    if (className == "LocalScript") return "localscript";
    if (className == "ModuleScript") return "modulescript";
    return "";
}

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
static void *gPollHandle = NULL;
#define POLL_BACKOFF_MAX_MS 16000
static int gConsecutivePollFailures = 0;

void DiscordPresence_Tick(void) {
    if (!gStarted || !VM_IsReady()) return;
    uint64_t now = monotonicMillis();
    void *currentL = VM_GetLuaState();
    if (currentL != gLastPollLuaState) {
        gLastPollLuaState = currentL;
        gLastPollMs = 0;
        gPollHandle = NULL;
        gConsecutivePollFailures = 0;
    }
    uint64_t effectiveIntervalMs = POLL_INTERVAL_MS;
    for (int i = 0; i < gConsecutivePollFailures && effectiveIntervalMs < POLL_BACKOFF_MAX_MS; i++) {
        effectiveIntervalMs *= 2;
    }
    if (effectiveIntervalMs > POLL_BACKOFF_MAX_MS) effectiveIntervalMs = POLL_BACKOFF_MAX_MS;
    if (now - gLastPollMs < effectiveIntervalMs) return;
    gLastPollMs = now;

    if (!gPollHandle) {
        gPollHandle = LuauRuntime_LoadPersistent(POLL_SETUP_SCRIPT, "=DiscordPresencePoll");
        if (!gPollHandle) {
            gConsecutivePollFailures++;
            return;
        }
    }

    std::string result;
    if (!LuauRuntime_CallPersistent(gPollHandle, result)) {
        gPollHandle = NULL;
        gConsecutivePollFailures++;
        return;
    }
    gConsecutivePollFailures = 0;

    std::string fields[6];
    splitTabFields(result, fields, 6);
    const std::string &placeId = fields[0];
    if (placeId.empty() || placeId.find_first_not_of("0123456789") != std::string::npos) return;
    const std::string &scriptName = fields[1];
    const std::string &scriptClass = fields[2];
    const std::string &startLine = fields[3];
    const std::string &startCharacter = fields[4];
    bool isRunning = fields[5] == "true" || VM_IsPlayTestActive();
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

        if (resolved && isPublic) joinUrl = "https://www.roblox.com/games/" + placeId;

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
