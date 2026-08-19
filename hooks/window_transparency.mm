#import <Cocoa/Cocoa.h>
#include <stdio.h>
#include <stdarg.h>
#include <algorithm>
#include <cctype>
#include <cstdlib>
#include <string>
#include <vector>

static FILE *gLog = NULL;
static void logmsg(const char *fmt, ...) {
    if (!gLog) return;
    va_list args;
    va_start(args, fmt);
    vfprintf(gLog, fmt, args);
    va_end(args);
    fflush(gLog);
}

static const char *WINDOW_TRANSPARENCY_CONFIG_PATH = "/Users/Shared/rbx-theme-set/WindowTransparency.json";

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

static bool jsonExtractString(const std::string &json, const char *key, std::string &out) {
    size_t pos;
    if (!jsonFindValueStart(json, key, pos)) return false;
    if (pos >= json.size() || json[pos] != '"') return false;
    pos++;
    size_t end = pos;
    while (end < json.size() && json[end] != '"') {
        if (json[end] == '\\') end++;
        end++;
    }
    if (end >= json.size()) return false;
    out = json.substr(pos, end - pos);
    return true;
}

static bool jsonExtractNumber(const std::string &json, const char *key, double &out) {
    size_t pos;
    if (!jsonFindValueStart(json, key, pos)) return false;
    char *endPtr = NULL;
    double v = strtod(json.c_str() + pos, &endPtr);
    if (endPtr == json.c_str() + pos) return false;
    out = v;
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

struct HotkeySpec {
    NSEventModifierFlags modifiers = 0;
    bool useKeyCode = false;
    unsigned short keyCode = 0;
    std::string character;
};

static std::vector<std::string> splitHotkey(const std::string &spec) {
    std::vector<std::string> parts;
    size_t pos = 0;
    while (pos <= spec.size()) {
        size_t plus = spec.find('+', pos);
        std::string part = (plus == std::string::npos) ? spec.substr(pos) : spec.substr(pos, plus - pos);
        if (!part.empty()) parts.push_back(part);
        if (plus == std::string::npos) break;
        pos = plus + 1;
    }
    return parts;
}

static bool parseHotkey(const std::string &specRaw, HotkeySpec &out) {
    std::string spec = specRaw;
    std::transform(spec.begin(), spec.end(), spec.begin(), ::tolower);
    std::vector<std::string> parts = splitHotkey(spec);
    if (parts.empty()) return false;

    std::string keyPart = parts.back();
    out = HotkeySpec{};
    for (size_t i = 0; i + 1 < parts.size(); i++) {
        const std::string &m = parts[i];
        if (m == "cmd" || m == "command") out.modifiers |= NSEventModifierFlagCommand;
        else if (m == "option" || m == "alt") out.modifiers |= NSEventModifierFlagOption;
        else if (m == "control" || m == "ctrl") out.modifiers |= NSEventModifierFlagControl;
        else if (m == "shift") out.modifiers |= NSEventModifierFlagShift;
    }

    if (keyPart == "up") { out.useKeyCode = true; out.keyCode = 126; }
    else if (keyPart == "down") { out.useKeyCode = true; out.keyCode = 125; }
    else if (keyPart == "left") { out.useKeyCode = true; out.keyCode = 123; }
    else if (keyPart == "right") { out.useKeyCode = true; out.keyCode = 124; }
    else if (keyPart == "=" || keyPart == "+") { out.useKeyCode = true; out.keyCode = 24; out.character = "="; }
    else if (keyPart == "-") { out.useKeyCode = true; out.keyCode = 27; out.character = "-"; }
    else if (keyPart.size() >= 1) { out.character = keyPart; }
    else return false;
    return true;
}

static bool hotkeyMatches(const HotkeySpec &hk, NSEvent *event) {
    if (hk.modifiers == 0 && !hk.useKeyCode && hk.character.empty()) return false;
    NSEventModifierFlags mods = event.modifierFlags & NSEventModifierFlagDeviceIndependentFlagsMask;

    if (hk.useKeyCode && mods == hk.modifiers && event.keyCode == hk.keyCode) return true;

    if (!hk.character.empty()) {
        NSEventModifierFlags modsNoShift = mods & ~NSEventModifierFlagShift;
        NSEventModifierFlags wantNoShift = hk.modifiers & ~NSEventModifierFlagShift;
        if (modsNoShift == wantNoShift) {
            NSString *chars = [event.charactersIgnoringModifiers lowercaseString];
            if (chars && [chars UTF8String] && hk.character == [chars UTF8String]) return true;
        }
    }
    return false;
}

#define DEFAULT_OPACITY 1.0
#define DEFAULT_STEP 0.05
#define DEFAULT_MIN_OPACITY 0.2
#define DEFAULT_INCREASE_HOTKEY "ctrl+="
#define DEFAULT_DECREASE_HOTKEY "ctrl+-"

static double gOpacity = DEFAULT_OPACITY;
static double gStep = DEFAULT_STEP;
static double gMinOpacity = DEFAULT_MIN_OPACITY;
static HotkeySpec gIncreaseHotkey;
static HotkeySpec gDecreaseHotkey;

static void applyOpacity(void) {
    for (NSWindow *win in [NSApp windows]) {
        [win setAlphaValue:gOpacity];
    }
    logmsg("applied opacity=%.2f to %lu window(s)\n", gOpacity, (unsigned long)[[NSApp windows] count]);
}

__attribute__((constructor))
static void bootstrap(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        gLog = fopen("/tmp/studio_patcher_transparency.txt", "w");

        std::string configJson;
        if (!readFile(WINDOW_TRANSPARENCY_CONFIG_PATH, configJson)) {
            logmsg("no %s - window transparency hook not installed\n", WINDOW_TRANSPARENCY_CONFIG_PATH);
            return;
        }

        bool enabled = true;
        jsonExtractBool(configJson, "enabled", enabled);
        if (!enabled) {
            logmsg("WindowTransparency disabled in %s - hook not installed\n", WINDOW_TRANSPARENCY_CONFIG_PATH);
            return;
        }

        jsonExtractNumber(configJson, "opacity", gOpacity);
        jsonExtractNumber(configJson, "step", gStep);
        jsonExtractNumber(configJson, "minOpacity", gMinOpacity);
        if (gOpacity < gMinOpacity) gOpacity = gMinOpacity;
        if (gOpacity > 1.0) gOpacity = 1.0;

        std::string increaseSpec = DEFAULT_INCREASE_HOTKEY, decreaseSpec = DEFAULT_DECREASE_HOTKEY;
        jsonExtractString(configJson, "increaseHotkey", increaseSpec);
        jsonExtractString(configJson, "decreaseHotkey", decreaseSpec);
        if (!parseHotkey(increaseSpec, gIncreaseHotkey)) parseHotkey(DEFAULT_INCREASE_HOTKEY, gIncreaseHotkey);
        if (!parseHotkey(decreaseSpec, gDecreaseHotkey)) parseHotkey(DEFAULT_DECREASE_HOTKEY, gDecreaseHotkey);

        applyOpacity();

        [NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskKeyDown
                                                handler:^NSEvent *(NSEvent *event) {
            if (hotkeyMatches(gIncreaseHotkey, event)) {
                gOpacity += gStep;
                if (gOpacity > 1.0) gOpacity = 1.0;
                applyOpacity();
                return nil;
            }
            if (hotkeyMatches(gDecreaseHotkey, event)) {
                gOpacity -= gStep;
                if (gOpacity < gMinOpacity) gOpacity = gMinOpacity;
                applyOpacity();
                return nil;
            }
            return event;
        }];

        logmsg("window transparency hook installed, opacity=%.2f step=%.2f min=%.2f increase=%s decrease=%s\n",
               gOpacity, gStep, gMinOpacity, increaseSpec.c_str(), decreaseSpec.c_str());
    });
}
