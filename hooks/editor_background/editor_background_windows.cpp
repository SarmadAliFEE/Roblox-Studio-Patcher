#include <windows.h>
#include <dbghelp.h>
#include <cctype>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>

#pragma comment(lib, "dbghelp.lib")

static const char *EDITOR_BACKGROUND_CONFIG_PATH = "C:\\Users\\Public\\rbxthemeset\\EditorBackground.json";
static const char *EDITOR_BACKGROUND_LOG_PATH = "C:\\Users\\Public\\rbxthemeset\\editor_background_log.txt";

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

static int countOccurrences(const char *haystack, const char *needle) {
    int count = 0;
    const char *p = haystack;
    while ((p = strstr(p, needle)) != NULL) {
        count++;
        p += strlen(needle);
    }
    return count;
}

static void *findExportPrecise(const char *label, HMODULE mod, const char *const *requiredSubstrings, int requiredCount,
                                const char *mustAppearTwice) {
    if (!mod) return NULL;
    BYTE *base = (BYTE *)mod;
    IMAGE_DOS_HEADER *dos = (IMAGE_DOS_HEADER *)base;
    IMAGE_NT_HEADERS *nt = (IMAGE_NT_HEADERS *)(base + dos->e_lfanew);
    IMAGE_DATA_DIRECTORY &dir = nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_EXPORT];
    if (!dir.VirtualAddress) return NULL;
    IMAGE_EXPORT_DIRECTORY *exports = (IMAGE_EXPORT_DIRECTORY *)(base + dir.VirtualAddress);
    DWORD *names = (DWORD *)(base + exports->AddressOfNames);
    WORD *ordinals = (WORD *)(base + exports->AddressOfNameOrdinals);
    DWORD *functions = (DWORD *)(base + exports->AddressOfFunctions);

    char undecorated[1024];
    void *match = NULL;
    int matchCount = 0;
    for (DWORD i = 0; i < exports->NumberOfNames; i++) {
        const char *mangled = (const char *)(base + names[i]);
        DWORD rva = functions[ordinals[i]];
        void *addr = (void *)(base + rva);
        if (UnDecorateSymbolName(mangled, undecorated, sizeof(undecorated), UNDNAME_COMPLETE) == 0) continue;

        bool ok = true;
        for (int j = 0; j < requiredCount; j++) {
            if (!strstr(undecorated, requiredSubstrings[j])) {
                ok = false;
                break;
            }
        }
        if (ok && mustAppearTwice && countOccurrences(undecorated, mustAppearTwice) < 2) ok = false;
        if (!ok) continue;

        matchCount++;
        if (!match) match = addr;
        logmsg("  [%s] candidate #%d: %s\n", label, matchCount, undecorated);
    }
    if (matchCount > 1) logmsg("WARNING: [%s] %d candidates matched, used the first listed above\n", label, matchCount);
    return match;
}

static void *findExport(const char *label, HMODULE mod, const char *needle) {
    return findExportPrecise(label, mod, &needle, 1, NULL);
}

static HMODULE loadQtModule(const char *const *candidates, int count) {
    for (int i = 0; i < count; i++) {
        HMODULE m = GetModuleHandleA(candidates[i]);
        if (m) return m;
    }
    return NULL;
}

struct QListData {
    int32_t ref;
    int32_t alloc;
    int32_t begin;
    int32_t end;
    void *array[1];
};

typedef void *(*AllWidgetsFn)(void *outStorage);
typedef const char *(*ClassNameFn)(const void *thisMeta);
typedef void *(*ViewportFn)(void *thisWidget);
typedef void (*FromUtf8Fn)(void *outStorage, const char *str, int len);
typedef void (*PixmapCtorFn)(void *thisPixmap, const void *qstringRef, const char *fmt, uint32_t flags);
typedef void (*PixmapDtorFn)(void *thisPixmap);
typedef int (*PixmapDimFn)(const void *thisPixmap);
typedef void (*PainterCtorFn)(void *thisPainter, void *device);
typedef void (*PainterDtorFn)(void *thisPainter);
typedef void (*SetOpacityFn)(void *thisPainter, double opacity);
typedef void (*DrawPixmapRectFn)(void *thisPainter, const void *destRectF, const void *pixmapRef, const void *srcRectF);
typedef bool (*PainterIsActiveFn)(const void *thisPainter);

struct QSizeRaw {
    int32_t w, h;
};
struct QRectRaw {
    int32_t x1, y1, x2, y2;
};

typedef void (*PixmapScaledFn)(void *thisPixmap, void *sretOut, const void *qsize, int aspectMode, int transformMode);
typedef void (*FrameGeometryFn)(void *thisWidget, void *sretOut);

#define PAINT_SLOT 46
#define QPAINTDEVICE_SUBOBJECT_OFFSET 16
#define MAX_HOOKED_VTABLES 16
#define DEFAULT_BACKGROUND_OPACITY 0.15

static unsigned char gPixmapStorage[128];
static bool gPixmapLoaded = false;
static unsigned char gScaledPixmapStorage[128];
static bool gScaledPixmapValid = false;
static int gScaledForW = -1;
static int gScaledForH = -1;
static double gBackgroundOpacity = DEFAULT_BACKGROUND_OPACITY;

static AllWidgetsFn gAllWidgetsFn = NULL;
static ClassNameFn gClassNameFn = NULL;
static ViewportFn gViewportFn = NULL;
static FromUtf8Fn gFromUtf8Fn = NULL;
static PixmapCtorFn gPixmapCtorFn = NULL;
static PixmapDtorFn gPixmapDtor = NULL;
static PixmapDimFn gPixmapWidth = NULL;
static PixmapDimFn gPixmapHeight = NULL;
static PixmapScaledFn gPixmapScaled = NULL;
static PainterCtorFn gPainterCtor = NULL;
static PainterDtorFn gPainterDtor = NULL;
static SetOpacityFn gSetOpacity = NULL;
static DrawPixmapRectFn gDrawPixmapRect = NULL;
static FrameGeometryFn gFrameGeometry = NULL;
static PainterIsActiveFn gPainterIsActive = NULL;

static void *gHookedVtables[MAX_HOOKED_VTABLES];
static void *gHookedOriginals[MAX_HOOKED_VTABLES];
static int gHookedCount = 0;

static void *originalForVtable(void *vtable) {
    for (int i = 0; i < gHookedCount; i++) {
        if (gHookedVtables[i] == vtable) return gHookedOriginals[i];
    }
    return NULL;
}

static const char *classNameOf(void *widget) {
    void *vtable = *(void **)widget;
    typedef void *(*MetaObjFn)(void *);
    MetaObjFn fn = (MetaObjFn)((void **)vtable)[0];
    void *metaObj = fn(widget);
    if (!metaObj) return NULL;
    return gClassNameFn(metaObj);
}

static int gPaintCallCount = 0;

static void paintDetour(void *self, void *event) {
    void *vtable = *(void **)self;
    void *original = originalForVtable(vtable);
    bool logThis = gPaintCallCount++ < 8;
    if (logThis) logmsg("paintDetour #%d self=%p vtable=%p original=%p\n", gPaintCallCount, self, vtable, original);
    if (original) {
        typedef void (*OrigFn)(void *, void *);
        ((OrigFn)original)(self, event);
    }

    if (logThis) {
        const char *selfName = NULL;
        selfName = classNameOf(self);
        logmsg("  classNameOf(self)=%s\n", selfName ? selfName : "(null)");
    }

    if (!gPixmapLoaded || !gPainterCtor || !gDrawPixmapRect) {
        if (logThis) logmsg("  bailed: pixmapLoaded=%d painterCtor=%p drawPixmapRect=%p\n", gPixmapLoaded, gPainterCtor, gDrawPixmapRect);
        return;
    }

    void *vp = gViewportFn ? gViewportFn(self) : NULL;
    void *widget = vp ? vp : self;
    if (logThis) logmsg("  vp=%p widget=%p\n", vp, widget);

    int pw = gPixmapWidth ? gPixmapWidth(gPixmapStorage) : 0;
    int ph = gPixmapHeight ? gPixmapHeight(gPixmapStorage) : 0;
    if (logThis) logmsg("  pw=%d ph=%d\n", pw, ph);

    double vw = 2000.0, vh = 2000.0;
    if (gFrameGeometry) {
        QRectRaw geo{};
        gFrameGeometry(widget, &geo);
        if (logThis) logmsg("  frameGeometry returned x1=%d y1=%d x2=%d y2=%d\n", geo.x1, geo.y1, geo.x2, geo.y2);
        double w = (double)(geo.x2 - geo.x1 + 1);
        double h = (double)(geo.y2 - geo.y1 + 1);
        if (w > 0 && h > 0 && w <= 16384.0 && h <= 16384.0) { vw = w; vh = h; }
    }

    int sw = (int)vw, sh = (int)vh;
    if (pw > 0 && ph > 0) {
        double scale = (vw / pw) > (vh / ph) ? (vw / pw) : (vh / ph);
        sw = (int)(pw * scale + 0.5);
        sh = (int)(ph * scale + 0.5);
    }
    if (sw < 1) sw = 1;
    if (sh < 1) sh = 1;
    if (sw > 16384) sw = 16384;
    if (sh > 16384) sh = 16384;

    if (gPixmapScaled && (!gScaledPixmapValid || sw != gScaledForW || sh != gScaledForH)) {
        if (gScaledPixmapValid && gPixmapDtor) gPixmapDtor(gScaledPixmapStorage);
        memset(gScaledPixmapStorage, 0, sizeof(gScaledPixmapStorage));
        QSizeRaw target{sw, sh};
        gPixmapScaled(gPixmapStorage, gScaledPixmapStorage, &target, 0, 1);
        gScaledPixmapValid = true;
        gScaledForW = sw;
        gScaledForH = sh;
    }

    void *drawSource = gScaledPixmapValid ? gScaledPixmapStorage : gPixmapStorage;
    int drawW = gScaledPixmapValid ? sw : pw;
    int drawH = gScaledPixmapValid ? sh : ph;

    void *target = (char *)widget + QPAINTDEVICE_SUBOBJECT_OFFSET;
    if (logThis) logmsg("  vw=%.0f vh=%.0f drawW=%d drawH=%d target=%p\n", vw, vh, drawW, drawH, target);

    unsigned char painterStorage[64];
    memset(painterStorage, 0, sizeof(painterStorage));
    gPainterCtor(painterStorage, target);

    if (logThis && gPainterIsActive) {
        bool active = gPainterIsActive(painterStorage);
        logmsg("  painter active=%d\n", (int)active);
    }

    if (gSetOpacity) gSetOpacity(painterStorage, gBackgroundOpacity);

    double destRect[4] = {(vw - drawW) / 2.0, (vh - drawH) / 2.0, (double)drawW, (double)drawH};
    double srcRect[4] = {0.0, 0.0, (double)drawW, (double)drawH};
    gDrawPixmapRect(painterStorage, destRect, drawSource, srcRect);

    if (gPainterDtor) gPainterDtor(painterStorage);
    if (logThis) logmsg("  paintDetour #%d done\n", gPaintCallCount);
}

static bool hookVtableForWidget(void *widget, void *vtable) {
    if (originalForVtable(vtable) != NULL) return false;
    if (gHookedCount >= MAX_HOOKED_VTABLES) return false;

    void **slot = &((void **)vtable)[PAINT_SLOT];
    void *original = *slot;

    DWORD oldProtect;
    if (!VirtualProtect(slot, sizeof(void *), PAGE_READWRITE, &oldProtect)) {
        logmsg("vtable VirtualProtect rw failed for vtable=%p\n", vtable);
        return false;
    }
    *slot = (void *)paintDetour;
    VirtualProtect(slot, sizeof(void *), oldProtect, &oldProtect);

    gHookedVtables[gHookedCount] = vtable;
    gHookedOriginals[gHookedCount] = original;
    gHookedCount++;

    logmsg("hooked NEW vtable=%p (widget=%p) slot[%d]=%p original=%p, total hooked=%d\n",
           vtable, widget, PAINT_SLOT, slot, original, gHookedCount);
    return true;
}

static bool hasValidViewport(void *widget, QListData *allWidgets) {
    if (!gViewportFn) return false;
    void *vp = gViewportFn(widget);
    if (!vp || vp == widget) return false;
    for (int i = allWidgets->begin; i < allWidgets->end; i++) {
        if (allWidgets->array[i] == vp) return true;
    }
    return false;
}

static void tryInstallHook() {
    unsigned char resultStorage[8];
    void *ret = gAllWidgetsFn(resultStorage);
    QListData *result = *(QListData **)ret;
    if (!result) return;

    for (int i = result->begin; i < result->end; i++) {
        void *widget = result->array[i];
        if (!widget) continue;
        const char *name = classNameOf(widget);
        if (!name || !strstr(name, "ScriptEditor")) continue;
        if (!hasValidViewport(widget, result)) continue;

        void *vtable = *(void **)widget;
        hookVtableForWidget(widget, vtable);
    }
}

static const char *QT_CORE_NAMES[] = {"Qt5Core.dll", "Qt5Cored.dll"};
static const char *QT_WIDGETS_NAMES[] = {"Qt5Widgets.dll", "Qt5Widgetsd.dll"};
static const char *QT_GUI_NAMES[] = {"Qt5Gui.dll", "Qt5Guid.dll"};

static std::string gImagePathStr;
static bool gSymbolsResolved = false;
static UINT_PTR gTimerId = 0;

static bool resolveSymbols() {
    HMODULE core = loadQtModule(QT_CORE_NAMES, 2);
    HMODULE widgets = loadQtModule(QT_WIDGETS_NAMES, 2);
    HMODULE gui = loadQtModule(QT_GUI_NAMES, 2);
    if (!core || !widgets || !gui) return false;

    gAllWidgetsFn = (AllWidgetsFn)findExport("allWidgets", widgets, "QApplication::allWidgets");
    gClassNameFn = (ClassNameFn)findExport("className", core, "QMetaObject::className");
    static const char *viewportReqs[] = {"QAbstractScrollArea::viewport(void)"};
    gViewportFn = (ViewportFn)findExportPrecise("viewport", widgets, viewportReqs, 1, NULL);

    static const char *fromUtf8Reqs[] = {"QString::fromUtf8(", "char const"};
    gFromUtf8Fn = (FromUtf8Fn)findExportPrecise("fromUtf8", core, fromUtf8Reqs, 2, NULL);

    static const char *pixmapCtorReqs[] = {"QPixmap::QPixmap", "class QString", "ImageConversionFlag"};
    gPixmapCtorFn = (PixmapCtorFn)findExportPrecise("pixmapCtor", gui, pixmapCtorReqs, 3, NULL);

    gPixmapDtor = (PixmapDtorFn)findExport("pixmapDtor", gui, "QPixmap::~QPixmap");
    gPixmapWidth = (PixmapDimFn)findExport("pixmapWidth", gui, "QPixmap::width");
    gPixmapHeight = (PixmapDimFn)findExport("pixmapHeight", gui, "QPixmap::height");

    static const char *pixmapScaledReqs[] = {"QPixmap::scaled", "QSize"};
    gPixmapScaled = (PixmapScaledFn)findExportPrecise("pixmapScaled", gui, pixmapScaledReqs, 2, NULL);

    static const char *painterCtorReqs[] = {"QPainter::QPainter", "QPaintDevice"};
    gPainterCtor = (PainterCtorFn)findExportPrecise("painterCtor", gui, painterCtorReqs, 2, NULL);

    gPainterIsActive = (PainterIsActiveFn)findExport("painterIsActive", gui, "QPainter::isActive");

    gPainterDtor = (PainterDtorFn)findExport("painterDtor", gui, "QPainter::~QPainter");
    gSetOpacity = (SetOpacityFn)findExport("setOpacity", gui, "QPainter::setOpacity");

    static const char *drawPixmapReqs[] = {"QPainter::drawPixmap", "QPixmap"};
    gDrawPixmapRect = (DrawPixmapRectFn)findExportPrecise("drawPixmap", gui, drawPixmapReqs, 2, "QRectF");

    gFrameGeometry = (FrameGeometryFn)findExport("frameGeometry", widgets, "QWidget::frameGeometry");

    logmsg("syms: allWidgets=%p className=%p viewport=%p fromUtf8=%p pixmapCtor=%p pixmapDtor=%p pw=%p ph=%p "
           "scaled=%p painterCtor=%p painterDtor=%p setOpacity=%p drawRect=%p frameGeometry=%p\n",
           gAllWidgetsFn, gClassNameFn, gViewportFn, gFromUtf8Fn, gPixmapCtorFn, gPixmapDtor, gPixmapWidth,
           gPixmapHeight, gPixmapScaled, gPainterCtor, gPainterDtor, gSetOpacity, gDrawPixmapRect, gFrameGeometry);

    return gAllWidgetsFn && gClassNameFn && gViewportFn && gFromUtf8Fn && gPixmapCtorFn &&
           gPixmapWidth && gPixmapHeight && gPainterCtor && gPainterDtor && gDrawPixmapRect;
}

static void hexdump(const char *label, const void *data, size_t len) {
    const unsigned char *b = (const unsigned char *)data;
    std::string out;
    char byte[4];
    for (size_t i = 0; i < len; i++) {
        snprintf(byte, sizeof(byte), "%02x ", b[i]);
        out += byte;
    }
    logmsg("%s: %s\n", label, out.c_str());
}

static bool loadPixmap() {
    unsigned char pathStorage[16];
    memset(pathStorage, 0, sizeof(pathStorage));
    gFromUtf8Fn(pathStorage, gImagePathStr.c_str(), (int)gImagePathStr.size());
    hexdump("qstring bytes", pathStorage, sizeof(pathStorage));

    memset(gPixmapStorage, 0, sizeof(gPixmapStorage));
    gPixmapCtorFn(gPixmapStorage, pathStorage, NULL, 0);
    hexdump("qpixmap bytes", gPixmapStorage, sizeof(gPixmapStorage));

    int pw = gPixmapWidth(gPixmapStorage);
    int ph = gPixmapHeight(gPixmapStorage);
    logmsg("pixmap loaded from %s, size=%dx%d, opacity=%.2f\n", gImagePathStr.c_str(), pw, ph, gBackgroundOpacity);
    return pw > 0 && ph > 0;
}

static void CALLBACK timerProc(HWND, UINT, UINT_PTR, DWORD) {
    if (!gSymbolsResolved) {
        gSymbolsResolved = resolveSymbols();
    }
    if (gSymbolsResolved && !gPixmapLoaded) {
        gPixmapLoaded = loadPixmap();
    }
    if (gSymbolsResolved && gPixmapLoaded) {
        tryInstallHook();
    }
}

static void bootstrap() {
    gLog = fopen(EDITOR_BACKGROUND_LOG_PATH, "w");

    std::string configJson;
    if (!readFile(EDITOR_BACKGROUND_CONFIG_PATH, configJson)) {
        logmsg("no %s - hook not installed\n", EDITOR_BACKGROUND_CONFIG_PATH);
        return;
    }

    bool enabled = true;
    jsonExtractBool(configJson, "enabled", enabled);
    if (!enabled || !jsonExtractString(configJson, "image", gImagePathStr) || gImagePathStr.empty()) {
        logmsg("EditorBackground missing/disabled/no image - hook not installed\n");
        return;
    }
    jsonExtractNumber(configJson, "opacity", gBackgroundOpacity);
    if (gBackgroundOpacity < 0.0) gBackgroundOpacity = 0.0;
    if (gBackgroundOpacity > 1.0) gBackgroundOpacity = 1.0;

    gTimerId = SetTimer(NULL, 0, 1000, timerProc);
}

extern "C" __declspec(dllexport) void RSPHookInit() {}

BOOL APIENTRY DllMain(HMODULE hModule, DWORD reason, LPVOID) {
    if (reason == DLL_PROCESS_ATTACH) {
        DisableThreadLibraryCalls(hModule);
        bootstrap();
    }
    return TRUE;
}
