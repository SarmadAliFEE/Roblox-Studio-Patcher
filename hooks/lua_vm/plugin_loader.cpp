#include "plugin_loader.h"
#include "luau_runtime.h"
#include "vm_platform.h"

#include <atomic>
#include <chrono>
#include <cstdio>
#include <mutex>
#include <string>
#include <thread>

static const char *RUN_SCRIPT_INPUT_PATH = "/Users/Shared/rbx-theme-set/RunScript.luau";
static const char *RUN_SCRIPT_STATUS_PATH = "/Users/Shared/rbx-theme-set/RunScript.status.txt";
static const char *EXPOSE_STUDIO_SERVICE_MARKER_PATH = "/Users/Shared/rbx-theme-set/ExposeStudioService.trigger";
static const char *EXPOSE_STUDIO_SERVICE_STATUS_PATH = "/Users/Shared/rbx-theme-set/ExposeStudioService.status.txt";
#define WATCH_POLL_MS 250

static std::mutex gMutex;
static std::string gPendingSource;
static std::atomic<bool> gHasPending{false};
static std::atomic<bool> gHasExposeStudioServicePending{false};

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

static void writeStatusTo(const char *path, const std::string &status) {
    FILE *f = fopen(path, "w");
    if (!f) return;
    fwrite(status.data(), 1, status.size(), f);
    fclose(f);
}

static void writeStatus(const std::string &status) {
    writeStatusTo(RUN_SCRIPT_STATUS_PATH, status);
}

static void watcherLoop(void) {
    VM_Log("plugin_loader: watcher thread started\n");
    for (;;) {
        std::this_thread::sleep_for(std::chrono::milliseconds(WATCH_POLL_MS));

        if (!gHasExposeStudioServicePending.load(std::memory_order_acquire)) {
            FILE *marker = fopen(EXPOSE_STUDIO_SERVICE_MARKER_PATH, "rb");
            if (marker) {
                fclose(marker);
                remove(EXPOSE_STUDIO_SERVICE_MARKER_PATH);
                VM_Log("plugin_loader: watcher saw ExposeStudioService trigger, queuing\n");
                gHasExposeStudioServicePending.store(true, std::memory_order_release);
            }
        }

        if (gHasPending.load(std::memory_order_acquire)) continue;

        std::string source;
        if (!readFile(RUN_SCRIPT_INPUT_PATH, source)) continue;
        remove(RUN_SCRIPT_INPUT_PATH);
        VM_Log("plugin_loader: watcher read %zu bytes, queuing\n", source.size());

        {
            std::lock_guard<std::mutex> lock(gMutex);
            gPendingSource = std::move(source);
        }
        gHasPending.store(true, std::memory_order_release);
    }
}

void PluginLoader_Start(void) {
    VM_Log("plugin_loader: PluginLoader_Start called\n");
    static std::thread watcher(watcherLoop);
    watcher.detach();
}

void PluginLoader_RunPendingIfAny(void) {
    if (gHasExposeStudioServicePending.load(std::memory_order_acquire)) {
        gHasExposeStudioServicePending.store(false, std::memory_order_release);
        writeStatusTo(EXPOSE_STUDIO_SERVICE_STATUS_PATH, "disabled: VM_PushInstance hangs, see vm_platform_mac.mm\n");
    }

    if (!gHasPending.load(std::memory_order_acquire)) return;
    VM_Log("plugin_loader: pending detected on VM thread, VM_IsReady=%d\n", (int)VM_IsReady());
    if (!VM_IsReady()) return; // leave it queued until the VM layer is ready

    std::string source;
    {
        std::lock_guard<std::mutex> lock(gMutex);
        source = std::move(gPendingSource);
        gPendingSource.clear();
    }
    gHasPending.store(false, std::memory_order_release);

    VM_Log("plugin_loader: running %zu bytes of source\n", source.size());
    std::string result;
    bool ok = LuauRuntime_Run(source, "=RunScript", result);
    VM_Log("plugin_loader: LuauRuntime_Run returned %d, result=%s\n", (int)ok, result.c_str());
    writeStatus((ok ? "ok\n" : "failed\n") + result + "\n");
}
