#import <Foundation/Foundation.h>
#include <dlfcn.h>
#include <pthread.h>
#include <stdint.h>
#include <string.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>
#include <stdarg.h>
#include <cctype>
#include <cstdlib>
#include <string>

struct QListData {
    int32_t ref;
    int32_t alloc;
    int32_t begin;
    int32_t end;
    void *array[1];
};

struct IndirectResult8 {
    unsigned char storage[8];
    IndirectResult8() = default;
    IndirectResult8(const IndirectResult8 &) = default;
    ~IndirectResult8() {}
};

typedef IndirectResult8 (*AllWidgetsFn)();
typedef void *(*ClassNameFn)(const void *metaObject);
typedef IndirectResult8 (*FromUtf8Fn)(const char *, int);
typedef void *(*ViewportFn)(void *);
typedef void (*PixmapCtorFn)(void *, const void *, const char *, uint32_t);
typedef void (*PixmapDtorFn)(void *);
typedef int (*PixmapDimFn)(const void *);
typedef void (*PainterCtorFn)(void *, void *);
typedef void (*PainterDtorFn)(void *);
typedef void (*DrawPixmapRectFn)(void *, const void *, const void *, const void *);
typedef void (*SetOpacityFn)(void *, double);
typedef void (*SetRenderHintFn)(void *, int, bool);

struct QSizeRaw {
    int32_t w, h;
};

struct IndirectResultPixmap {
    unsigned char storage[128];
    IndirectResultPixmap() = default;
    IndirectResultPixmap(const IndirectResultPixmap &) = default;
    ~IndirectResultPixmap() {}
};
typedef IndirectResultPixmap (*PixmapScaledFn)(void *thisPixmap, const void *qsize, int aspectMode, int transformMode);

struct QRectRaw {
    int32_t x1, y1, x2, y2;
};
typedef QRectRaw (*FrameGeometryFn)(void *);

static FILE *gLog = NULL;
static void logmsg(const char *fmt, ...) {
    if (!gLog) return;
    va_list args;
    va_start(args, fmt);
    vfprintf(gLog, fmt, args);
    va_end(args);
    fflush(gLog);
}

static const char *EDITOR_BACKGROUND_CONFIG_PATH = "/Users/Shared/rbx-theme-set/EditorBackground.json";

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

static const char *classNameOf(void *widget, ClassNameFn classNameFn) {
    void *vtable = *(void **)widget;
    typedef void *(*MetaObjFn)(void *);
    MetaObjFn fn = (MetaObjFn)((void **)vtable)[0];
    void *metaObj = fn(widget);
    if (!metaObj) return NULL;
    return (const char *)classNameFn(metaObj);
}

static uintptr_t buildLdrBrStub(uint8_t *at, uintptr_t target) {
    uint32_t ldr = 0x58000051;
    uint32_t br = 0xd61f0220;
    memcpy(at, &ldr, 4);
    memcpy(at + 4, &br, 4);
    memcpy(at + 8, &target, 8);
    return (uintptr_t)at;
}

#define PAINT_SLOT 30
#define QPAINTDEVICE_SUBOBJECT_OFFSET 16
#define MAX_HOOKED_VTABLES 16

static unsigned char gPixmapStorage[128];
static bool gPixmapLoaded = false;

static unsigned char gScaledPixmapStorage[128];
static bool gScaledPixmapValid = false;
static int gScaledForW = -1;
static int gScaledForH = -1;

static PainterCtorFn gPainterCtor = NULL;
static PainterDtorFn gPainterDtor = NULL;
static DrawPixmapRectFn gDrawPixmapRect = NULL;
static SetOpacityFn gSetOpacity = NULL;
static SetRenderHintFn gSetRenderHint = NULL;
static FrameGeometryFn gFrameGeometry = NULL;
static PixmapDimFn gPixmapWidth = NULL;
static PixmapDimFn gPixmapHeight = NULL;
static PixmapDtorFn gPixmapDtor = NULL;
static PixmapScaledFn gPixmapScaled = NULL;
static ViewportFn gViewportFn = NULL;

#define RENDER_HINT_ANTIALIASING 0x01
#define RENDER_HINT_SMOOTH_PIXMAP_TRANSFORM 0x04
#define DEFAULT_BACKGROUND_OPACITY 0.15

static double gBackgroundOpacity = DEFAULT_BACKGROUND_OPACITY;

static void *gHookedVtables[MAX_HOOKED_VTABLES];
static void *gHookedOriginals[MAX_HOOKED_VTABLES];
static int gHookedCount = 0;

static void *originalForVtable(void *vtable) {
    for (int i = 0; i < gHookedCount; i++) {
        if (gHookedVtables[i] == vtable) return gHookedOriginals[i];
    }
    return NULL;
}

static void paintDetour(void *self, void *event) {
    void *vtable = *(void **)self;
    void *original = originalForVtable(vtable);
    if (original) {
        typedef void (*OrigFn)(void *, void *);
        ((OrigFn)original)(self, event);
    }

    if (gPixmapLoaded && gPainterCtor && gDrawPixmapRect) {
        void *vp = gViewportFn ? gViewportFn(self) : NULL;
        void *widget = vp ? vp : self;

        int pw = gPixmapWidth ? gPixmapWidth(gPixmapStorage) : 0;
        int ph = gPixmapHeight ? gPixmapHeight(gPixmapStorage) : 0;

        double vw = 2000.0, vh = 2000.0;
        if (gFrameGeometry) {
            QRectRaw geo = gFrameGeometry(widget);
            double w = (double)(geo.x2 - geo.x1 + 1);
            double h = (double)(geo.y2 - geo.y1 + 1);
            if (w > 0 && h > 0 && w <= 16384.0 && h <= 16384.0) { vw = w; vh = h; }
        }

        // "cover" fit: scale to fill the whole viewport, centered, cropping overflow.
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
            IndirectResultPixmap scaledResult = gPixmapScaled(gPixmapStorage, &target, 0 /* IgnoreAspectRatio */, 1 /* SmoothTransformation */);
            memcpy(gScaledPixmapStorage, scaledResult.storage, sizeof(gScaledPixmapStorage));
            gScaledPixmapValid = true;
            gScaledForW = sw;
            gScaledForH = sh;
        }

        void *drawSource = gScaledPixmapValid ? gScaledPixmapStorage : gPixmapStorage;
        int drawW = gScaledPixmapValid ? sw : pw;
        int drawH = gScaledPixmapValid ? sh : ph;

        void *target = (char *)widget + QPAINTDEVICE_SUBOBJECT_OFFSET;
        unsigned char painterStorage[64];
        memset(painterStorage, 0, sizeof(painterStorage));
        gPainterCtor(painterStorage, target);

        if (gSetOpacity) gSetOpacity(painterStorage, gBackgroundOpacity);

        double destRect[4] = {(vw - drawW) / 2.0, (vh - drawH) / 2.0, (double)drawW, (double)drawH};
        double srcRect[4] = {0.0, 0.0, (double)drawW, (double)drawH};
        gDrawPixmapRect(painterStorage, destRect, drawSource, srcRect);

        if (gPainterDtor) gPainterDtor(painterStorage);
    }
}

static AllWidgetsFn gAllWidgetsFn = NULL;
static ClassNameFn gClassNameFn = NULL;
static PixmapCtorFn gPixmapCtorFn = NULL;
static FromUtf8Fn gFromUtf8Fn = NULL;
static int gAttempt = 0;

static void tryInstallHook(void);

static void scheduleRetry(void) {
    gAttempt++;
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(1.0 * NSEC_PER_SEC)), dispatch_get_main_queue(), ^{
        tryInstallHook();
    });
}

static bool hookVtableForWidget(void *widget, void *vtable) {
    if (originalForVtable(vtable) != NULL) return false;
    if (gHookedCount >= MAX_HOOKED_VTABLES) return false;

    void **slot = &((void **)vtable)[PAINT_SLOT];
    void *original = *slot;

    size_t pageSize = (size_t)getpagesize();
    void *trampPage = mmap(NULL, pageSize, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    if (trampPage == MAP_FAILED) {
        logmsg("mmap failed for vtable=%p\n", vtable);
        return false;
    }
    buildLdrBrStub((uint8_t *)trampPage, (uintptr_t)paintDetour);
    mprotect(trampPage, pageSize, PROT_READ | PROT_EXEC);

    uintptr_t pageStart = (uintptr_t)vtable & ~(uintptr_t)(pageSize - 1);
    uintptr_t slotEnd = (uintptr_t)slot + 8;
    size_t protLen = ((slotEnd - pageStart) + pageSize - 1) & ~(pageSize - 1);
    if (mprotect((void *)pageStart, protLen, PROT_READ | PROT_WRITE) != 0) {
        logmsg("vtable mprotect rw failed for vtable=%p\n", vtable);
        return false;
    }
    *slot = trampPage;

    gHookedVtables[gHookedCount] = vtable;
    gHookedOriginals[gHookedCount] = original;
    gHookedCount++;

    logmsg("hooked NEW vtable=%p (widget=%p) slot[%d]=%p original=%p, total hooked=%d\n",
           vtable, widget, PAINT_SLOT, slot, original, gHookedCount);
    return true;
}

static bool hasValidViewport(void *widget, QListData *allWidgets) {
    if (!gViewportFn) return false;
    void *vp = NULL;
    @try {
        vp = gViewportFn(widget);
    } @catch (...) {
        return false;
    }
    if (!vp || vp == widget) return false;
    for (int i = allWidgets->begin; i < allWidgets->end; i++) {
        if (allWidgets->array[i] == vp) return true;
    }
    return false;
}

static void tryInstallHook(void) {
    IndirectResult8 raw = gAllWidgetsFn();
    QListData *result = NULL;
    memcpy(&result, raw.storage, sizeof(result));
    if (!result) {
        scheduleRetry();
        return;
    }

    for (int i = result->begin; i < result->end; i++) {
        void *widget = result->array[i];
        if (!widget) continue;
        const char *name = NULL;
        @try {
            name = classNameOf(widget, gClassNameFn);
        } @catch (...) {
            continue;
        }
        if (!name || !strstr(name, "ScriptEditor")) continue;
        if (!hasValidViewport(widget, result)) continue;

        void *vtable = *(void **)widget;
        hookVtableForWidget(widget, vtable);
    }

    scheduleRetry();
}

__attribute__((constructor))
static void bootstrap(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        gLog = fopen("/tmp/studio_patcher_bg2.txt", "w");

        std::string configJson;
        std::string imagePathStr;
        if (!readFile(EDITOR_BACKGROUND_CONFIG_PATH, configJson)) {
            logmsg("no %s - editor background hook not installed, studio paints normally\n", EDITOR_BACKGROUND_CONFIG_PATH);
            return;
        }

        bool enabled = true;
        jsonExtractBool(configJson, "enabled", enabled);
        if (!enabled || !jsonExtractString(configJson, "image", imagePathStr) || imagePathStr.empty()) {
            logmsg("EditorBackground in %s missing/disabled/no image - hook not installed, studio paints normally\n", EDITOR_BACKGROUND_CONFIG_PATH);
            return;
        }
        jsonExtractNumber(configJson, "opacity", gBackgroundOpacity);
        if (gBackgroundOpacity < 0.0) gBackgroundOpacity = 0.0;
        if (gBackgroundOpacity > 1.0) gBackgroundOpacity = 1.0;

        void *allWidgetsSym = dlsym(RTLD_DEFAULT, "_ZN12QApplication10allWidgetsEv");
        void *classNameSym = dlsym(RTLD_DEFAULT, "_ZNK11QMetaObject9classNameEv");
        void *viewportSym = dlsym(RTLD_DEFAULT, "_ZNK19QAbstractScrollArea8viewportEv");
        void *pixmapCtorSym = dlsym(RTLD_DEFAULT, "_ZN7QPixmapC1ERK7QStringPKc6QFlagsIN2Qt19ImageConversionFlagEE");
        void *fromUtf8Sym = dlsym(RTLD_DEFAULT, "_ZN7QString15fromUtf8_helperEPKci");
        void *pixmapWidthSym = dlsym(RTLD_DEFAULT, "_ZNK7QPixmap5widthEv");
        void *pixmapHeightSym = dlsym(RTLD_DEFAULT, "_ZNK7QPixmap6heightEv");
        void *painterCtorSym = dlsym(RTLD_DEFAULT, "_ZN8QPainterC1EP12QPaintDevice");
        void *painterDtorSym = dlsym(RTLD_DEFAULT, "_ZN8QPainterD1Ev");
        void *drawPixmapRectSym = dlsym(RTLD_DEFAULT, "_ZN8QPainter10drawPixmapERK6QRectFRK7QPixmapS2_");
        void *setOpacitySym = dlsym(RTLD_DEFAULT, "_ZN8QPainter10setOpacityEd");
        void *setRenderHintSym = dlsym(RTLD_DEFAULT, "_ZN8QPainter13setRenderHintENS_10RenderHintEb");
        void *frameGeometrySym = dlsym(RTLD_DEFAULT, "_ZNK7QWidget13frameGeometryEv");
        void *pixmapDtorSym = dlsym(RTLD_DEFAULT, "_ZN7QPixmapD1Ev");
        void *pixmapScaledSym = dlsym(RTLD_DEFAULT, "_ZNK7QPixmap6scaledERK5QSizeN2Qt15AspectRatioModeENS3_18TransformationModeE");

        logmsg("syms: allWidgets=%p className=%p viewport=%p pixmapCtor=%p fromUtf8=%p pw=%p ph=%p painterCtor=%p painterDtor=%p drawRect=%p setOpacity=%p setRenderHint=%p frameGeometry=%p\n",
               allWidgetsSym, classNameSym, viewportSym, pixmapCtorSym, fromUtf8Sym, pixmapWidthSym, pixmapHeightSym,
               painterCtorSym, painterDtorSym, drawPixmapRectSym, setOpacitySym, setRenderHintSym, frameGeometrySym);

        if (!allWidgetsSym || !classNameSym || !viewportSym || !pixmapCtorSym || !fromUtf8Sym ||
            !pixmapWidthSym || !pixmapHeightSym || !painterCtorSym || !painterDtorSym || !drawPixmapRectSym) {
            logmsg("missing symbol, aborting\n");
            return;
        }

        gAllWidgetsFn = (AllWidgetsFn)allWidgetsSym;
        gClassNameFn = (ClassNameFn)classNameSym;
        gViewportFn = (ViewportFn)viewportSym;
        gPixmapCtorFn = (PixmapCtorFn)pixmapCtorSym;
        gFromUtf8Fn = (FromUtf8Fn)fromUtf8Sym;
        gPixmapWidth = (PixmapDimFn)pixmapWidthSym;
        gPixmapHeight = (PixmapDimFn)pixmapHeightSym;
        gPainterCtor = (PainterCtorFn)painterCtorSym;
        gPainterDtor = (PainterDtorFn)painterDtorSym;
        gDrawPixmapRect = (DrawPixmapRectFn)drawPixmapRectSym;
        gSetOpacity = (SetOpacityFn)setOpacitySym;
        gSetRenderHint = (SetRenderHintFn)setRenderHintSym;
        gFrameGeometry = (FrameGeometryFn)frameGeometrySym;
        gPixmapDtor = (PixmapDtorFn)pixmapDtorSym;
        gPixmapScaled = (PixmapScaledFn)pixmapScaledSym;

        IndirectResult8 pathQString = gFromUtf8Fn(imagePathStr.c_str(), (int)imagePathStr.size());
        memset(gPixmapStorage, 0, sizeof(gPixmapStorage));
        gPixmapCtorFn(gPixmapStorage, pathQString.storage, NULL, 0);
        int pw = gPixmapWidth(gPixmapStorage);
        int ph = gPixmapHeight(gPixmapStorage);
        logmsg("pixmap loaded from %s, size=%dx%d, opacity=%.2f\n", imagePathStr.c_str(), pw, ph, gBackgroundOpacity);
        gPixmapLoaded = (pw > 0 && ph > 0);
        if (!gPixmapLoaded) {
            logmsg("image failed to load - hook not installed, studio paints normally\n");
            return;
        }

        tryInstallHook();
    });
}
