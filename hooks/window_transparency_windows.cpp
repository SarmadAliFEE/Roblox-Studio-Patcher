#include <windows.h>
#include <cctype>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

static const char *WINDOW_TRANSPARENCY_CONFIG_PATH = "C:\\Users\\Public\\rbxthemeset\\WindowTransparency.json";
static const char *WINDOW_TRANSPARENCY_LOG_PATH = "C:\\Users\\Public\\rbxthemeset\\window_transparency_log.txt";

static FILE *gLog = NULL;
static void logmsg(const char *fmt, ...) {
    if (!gLog) return;
    va_list args;
    va_start(args, fmt);
    vfprintf(gLog, fmt, args);
    va_end(args);
    fflush(gLog);
}

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
    bool ctrl = false, alt = false, shift = false, win = false;
    int vk = 0;
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
    for (auto &c : spec) c = (char)tolower((unsigned char)c);
    std::vector<std::string> parts = splitHotkey(spec);
    if (parts.empty()) return false;

    std::string keyPart = parts.back();
    out = HotkeySpec{};
    for (size_t i = 0; i + 1 < parts.size(); i++) {
        const std::string &m = parts[i];
        if (m == "ctrl" || m == "control") out.ctrl = true;
        else if (m == "alt" || m == "option") out.alt = true;
        else if (m == "shift") out.shift = true;
        else if (m == "win" || m == "windows" || m == "cmd") out.win = true;
    }

    if (keyPart == "up") out.vk = VK_UP;
    else if (keyPart == "down") out.vk = VK_DOWN;
    else if (keyPart == "left") out.vk = VK_LEFT;
    else if (keyPart == "right") out.vk = VK_RIGHT;
    else if (keyPart == "=" || keyPart == "+") out.vk = VK_OEM_PLUS;
    else if (keyPart == "-") out.vk = VK_OEM_MINUS;
    else if (keyPart.size() == 1) out.vk = (int)toupper((unsigned char)keyPart[0]);
    else return false;
    return out.vk != 0;
}

static bool hotkeyDown(const HotkeySpec &hk) {
    if (hk.vk == 0) return false;
    bool ctrl = (GetAsyncKeyState(VK_CONTROL) & 0x8000) != 0;
    bool alt = (GetAsyncKeyState(VK_MENU) & 0x8000) != 0;
    bool shift = (GetAsyncKeyState(VK_SHIFT) & 0x8000) != 0;
    bool win = (GetAsyncKeyState(VK_LWIN) & 0x8000) != 0 || (GetAsyncKeyState(VK_RWIN) & 0x8000) != 0;
    if (ctrl != hk.ctrl || alt != hk.alt || shift != hk.shift || win != hk.win) return false;
    return (GetAsyncKeyState(hk.vk) & 0x8000) != 0;
}

#define DEFAULT_OPACITY 1.0
#define DEFAULT_STEP 0.05
#define DEFAULT_MIN_OPACITY 0.2
#define DEFAULT_INCREASE_HOTKEY "alt+="
#define DEFAULT_DECREASE_HOTKEY "alt+-"
#define HOTKEY_POLL_MS 50

static double gOpacity = DEFAULT_OPACITY;
static double gStep = DEFAULT_STEP;
static double gMinOpacity = DEFAULT_MIN_OPACITY;
static HotkeySpec gIncreaseHotkey;
static HotkeySpec gDecreaseHotkey;
static UINT_PTR gTimerId = 0;
static bool gIncreaseWasDown = false;
static bool gDecreaseWasDown = false;

static BOOL CALLBACK applyOpacityToWindow(HWND hwnd, LPARAM opacityBytePtr) {
    DWORD pid = 0;
    GetWindowThreadProcessId(hwnd, &pid);
    if (pid != GetCurrentProcessId() || !IsWindowVisible(hwnd)) return TRUE;

    LONG_PTR exStyle = GetWindowLongPtrA(hwnd, GWL_EXSTYLE);
    if (exStyle & WS_EX_TOOLWINDOW) return TRUE;

    LONG_PTR style = GetWindowLongPtrA(hwnd, GWL_STYLE);
    if (!(style & WS_CAPTION)) return TRUE;

    if (!(exStyle & WS_EX_LAYERED)) {
        SetWindowLongPtrA(hwnd, GWL_EXSTYLE, exStyle | WS_EX_LAYERED);
    }
    SetLayeredWindowAttributes(hwnd, 0, (BYTE)opacityBytePtr, LWA_ALPHA);
    return TRUE;
}

static void applyOpacity(void) {
    BYTE alpha = (BYTE)(gOpacity * 255.0);
    EnumWindows(applyOpacityToWindow, (LPARAM)alpha);
    logmsg("applied opacity=%.2f (alpha=%d)\n", gOpacity, (int)alpha);
}

static void CALLBACK hotkeyPollProc(HWND, UINT, UINT_PTR, DWORD) {
    bool increaseDown = hotkeyDown(gIncreaseHotkey);
    if (increaseDown && !gIncreaseWasDown) {
        gOpacity += gStep;
        if (gOpacity > 1.0) gOpacity = 1.0;
        applyOpacity();
    }
    gIncreaseWasDown = increaseDown;

    bool decreaseDown = hotkeyDown(gDecreaseHotkey);
    if (decreaseDown && !gDecreaseWasDown) {
        gOpacity -= gStep;
        if (gOpacity < gMinOpacity) gOpacity = gMinOpacity;
        applyOpacity();
    }
    gDecreaseWasDown = decreaseDown;
}

static void bootstrap() {
    gLog = fopen(WINDOW_TRANSPARENCY_LOG_PATH, "w");

    std::string configJson;
    if (!readFile(WINDOW_TRANSPARENCY_CONFIG_PATH, configJson)) {
        logmsg("no %s - hook not installed\n", WINDOW_TRANSPARENCY_CONFIG_PATH);
        return;
    }

    bool enabled = true;
    jsonExtractBool(configJson, "enabled", enabled);
    if (!enabled) {
        logmsg("WindowTransparency disabled - hook not installed\n");
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
    gTimerId = SetTimer(NULL, 0, HOTKEY_POLL_MS, hotkeyPollProc);
    logmsg("window transparency hook installed, opacity=%.2f step=%.2f min=%.2f increase=%s decrease=%s\n",
           gOpacity, gStep, gMinOpacity, increaseSpec.c_str(), decreaseSpec.c_str());
}

extern "C" __declspec(dllexport) void RSPHookInit() {}

BOOL APIENTRY DllMain(HMODULE hModule, DWORD reason, LPVOID) {
    if (reason == DLL_PROCESS_ATTACH) {
        DisableThreadLibraryCalls(hModule);
        bootstrap();
    }
    return TRUE;
}
