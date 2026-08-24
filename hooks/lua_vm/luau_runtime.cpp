#include "luau_runtime.h"
#include "vm_platform.h"

#include <cstring>
#include <cstdio>
#include <cstdlib>
#include <mutex>
#include <vector>

#include "third_party/luau/Compiler/include/luacode.h"
#include "third_party/luau/Common/include/Luau/Common.h"
#include "third_party/luau/Common/include/Luau/ExperimentalFlags.h"

static inline bool looksLikeHeapPointer(uintptr_t v) {
    return v >= 0x1000 && v <= 0x7fffffffffffULL;
}

static std::mutex gVMCallMutex;

void LuauRuntime_Init(void) {
    for (Luau::FValue<bool> *flag = Luau::FValue<bool>::list; flag; flag = flag->next) {
        if (strncmp(flag->name, "Luau", 4) == 0 && !Luau::isAnalysisFlagExperimental(flag->name)) {
            flag->value = true;
        }
    }
}

static bool compileLuauSource(const std::string &source, std::vector<uint8_t> &outBytecode) {
    size_t outsize = 0;
    char *bytecode = luau_compile(source.c_str(), source.size(), NULL, &outsize);
    if (!bytecode || outsize == 0) {
        free(bytecode);
        return false;
    }
    outBytecode.assign(bytecode, bytecode + outsize);
    free(bytecode);
    // A leading 0 byte means compilation failed; the rest of the buffer is
    // a human-readable error message instead of real bytecode.
    return outBytecode[0] != 0;
}

static std::string describeTValue(uintptr_t addr) {
    uint8_t raw[16] = {0};
    if (!VM_SafeReadBytes((const void *)addr, raw, sizeof(raw))) return "<unreadable>";
    int32_t tt = 0;
    memcpy(&tt, raw + 12, 4);

    char buf[64];
    switch (tt) {
        case 0: return "nil";
        case 1: { int32_t b; memcpy(&b, raw, 4); return b ? "true" : "false"; }
        case 3: { double n; memcpy(&n, raw, 8); snprintf(buf, sizeof(buf), "%g", n); return buf; }
        case 4: { int32_t i; memcpy(&i, raw, 4); snprintf(buf, sizeof(buf), "%d", i); return buf; }
        case 6: { // LUA_TSTRING: value.gc -> TString{CommonHeader+pad, atom, next, hash, len@+20, data@+24}
            uintptr_t gc = 0;
            memcpy(&gc, raw, 8);
            uint32_t len = 0;
            if (!looksLikeHeapPointer(gc) || !VM_SafeReadBytes((const void *)(gc + 20), &len, sizeof(len))) {
                return "<string, unreadable>";
            }
            if (len > 4096) len = 4096;
            std::string s(len, '\0');
            if (!VM_SafeReadBytes((const void *)(gc + 24), &s[0], len)) return "<string, unreadable data>";
            return s;
        }
        case 7: return "<table>";
        case 8: return "<function>";
        case 9: return "<userdata>";
        case 10: return "<thread>";
        case 11: return "<buffer>";
        default: snprintf(buf, sizeof(buf), "<unknown type %d>", tt); return buf;
    }
}

static bool invokeAndDecode(void *L, VM_CallDispatchFn callFn, uintptr_t closureSlot, std::string &outResult, int nargs = 0) {
    uint64_t callResult = callFn(L, 0, nargs);
    VM_Log("luau_runtime: executed, call-dispatch returned 0x%llx\n", (unsigned long long)callResult);

    uintptr_t topAfter = 0;
    VM_SafeReadBytes((const uint8_t *)L + 0x58, &topAfter, sizeof(topAfter));

    bool ok = true;
    bool stateImplausible = false;
    size_t resultCount = 0;
    if (callResult != 0) {
        outResult = describeTValue(closureSlot);
        VM_Log("luau_runtime: script error: %s\n", outResult.c_str());
        ok = false;
    } else {
        #define MAX_DECODED_RESULTS 64
        #define MAX_PLAUSIBLE_RESULTS 256
        if (topAfter < closureSlot || (topAfter - closureSlot) / 0x10 > MAX_PLAUSIBLE_RESULTS) {
            VM_Log("luau_runtime: WARNING implausible topAfter=%p closureSlot=%p - treating as failure, not decoding\n",
                   (void *)topAfter, (void *)closureSlot);
            outResult = "<implausible result, call treated as failed>";
            ok = false;
            stateImplausible = true;
        } else {
            std::vector<std::string> results;
            for (uintptr_t slot = closureSlot;
                 slot + 0x10 <= topAfter && results.size() < MAX_DECODED_RESULTS;
                 slot += 0x10) {
                results.push_back(describeTValue(slot));
            }
            for (size_t i = 0; i < results.size(); i++) {
                if (i) outResult += "\t";
                outResult += results[i];
            }
            resultCount = results.size();
        }
    }
    
    if (stateImplausible) {
        VM_InvalidateCapturedState();
        return false;
    }

    VM_SafeWriteBytes((void *)((const uint8_t *)L + 0x58), &closureSlot, sizeof(closureSlot));

    if (!ok) return false;
    VM_Log("luau_runtime: %zu result(s): %s\n", resultCount, outResult.c_str());
    return true;
}

bool LuauRuntime_Run(const std::string &source, const char *chunkname, std::string &outResult) {
    std::lock_guard<std::mutex> lock(gVMCallMutex);
    if (!VM_IsReady()) {
        VM_Log("LuauRuntime_Run: VM not ready\n");
        return false;
    }
    void *L = VM_GetLuaState();

    std::vector<uint8_t> bytecode;
    if (!compileLuauSource(source, bytecode)) {
        VM_Log("LuauRuntime_Run: compile failed: %s\n", (const char *)bytecode.data() + 1);
        return false;
    }
    VM_Log("LuauRuntime_Run: compiled %zu bytes\n", bytecode.size());

    VM_LuauLoadFn loadFn = VM_GetLuauLoadFn();
    if (!loadFn) {
        VM_Log("LuauRuntime_Run: luau_load not resolved\n");
        return false;
    }
    VM_LuaNewthreadFn newthreadFn = VM_GetLuaNewthreadFn();
    VM_TaskDeferFn deferFn = VM_GetTaskDeferFn();
    if (newthreadFn && deferFn) {
        uintptr_t topBefore = 0;
        VM_SafeReadBytes((const uint8_t *)L + 0x58, &topBefore, sizeof(topBefore));

        void *freshL = newthreadFn(L);
        if (!freshL) {
            VM_Log("LuauRuntime_Run: lua_newthread returned null\n");
            return false;
        }

        VM_SafeWriteBytes((void *)((const uint8_t *)L + 0x58), &topBefore, sizeof(topBefore));

        VM_ElevateThreadCapabilities(freshL);

        int loadResult = loadFn(freshL, chunkname, bytecode.data(), bytecode.size(), 0);
        VM_Log("LuauRuntime_Run: load (fresh thread) returned %d\n", loadResult);
        if (loadResult != 0) return false;

        uintptr_t freshTop = 0;
        VM_SafeReadBytes((const uint8_t *)freshL + 0x58, &freshTop, sizeof(freshTop));
        uintptr_t freshClosurePtr = 0;
        VM_SafeReadBytes((const void *)(freshTop - 0x10), &freshClosurePtr, sizeof(freshClosurePtr));

        VM_ElevateClosureCapabilities((void *)freshClosurePtr);

        uint64_t deferResult = deferFn(freshL);
        VM_Log("luau_runtime: handed fresh thread to task_defer, result=0x%llx\n",
               (unsigned long long)deferResult);
        outResult = "<deferred>";
        return true;
    }

    int loadResult = loadFn(L, chunkname, bytecode.data(), bytecode.size(), 0);
    VM_Log("LuauRuntime_Run: load returned %d\n", loadResult);
    if (loadResult != 0) return false;

    VM_CallDispatchFn callFn = VM_GetCallDispatchFn();
    if (!callFn) {
        VM_Log("LuauRuntime_Run: call dispatch not resolved, loaded closure cannot be executed\n");
        return false;
    }

    uintptr_t topBefore = 0;
    VM_SafeReadBytes((const uint8_t *)L + 0x58, &topBefore, sizeof(topBefore));
    uintptr_t closureSlot = topBefore - 0x10;

    uintptr_t closurePtr = 0;
    VM_SafeReadBytes((const void *)closureSlot, &closurePtr, sizeof(closurePtr));
    VM_ElevateClosureCapabilities((void *)closurePtr);

    VM_ElevateSecurityContext();

    return invokeAndDecode(L, callFn, closureSlot, outResult);
}

bool LuauRuntime_RunSync(const std::string &source, const char *chunkname, std::string &outResult) {
    std::lock_guard<std::mutex> lock(gVMCallMutex);
    if (!VM_IsReady()) {
        VM_Log("LuauRuntime_RunSync: VM not ready\n");
        return false;
    }
    void *L = VM_GetLuaState();

    std::vector<uint8_t> bytecode;
    if (!compileLuauSource(source, bytecode)) {
        VM_Log("LuauRuntime_RunSync: compile failed: %s\n", (const char *)bytecode.data() + 1);
        return false;
    }
    VM_Log("LuauRuntime_RunSync: compiled %zu bytes\n", bytecode.size());

    VM_LuauLoadFn loadFn = VM_GetLuauLoadFn();
    VM_CallDispatchFn callFn = VM_GetCallDispatchFn();
    if (!loadFn || !callFn) {
        VM_Log("LuauRuntime_RunSync: luau_load/call dispatch not resolved\n");
        return false;
    }

    int loadResult = loadFn(L, chunkname, bytecode.data(), bytecode.size(), 0);
    VM_Log("LuauRuntime_RunSync: load returned %d\n", loadResult);
    if (loadResult != 0) return false;

    uintptr_t topBefore = 0;
    VM_SafeReadBytes((const uint8_t *)L + 0x58, &topBefore, sizeof(topBefore));
    uintptr_t closureSlot = topBefore - 0x10;

    uintptr_t closurePtr = 0;
    VM_SafeReadBytes((const void *)closureSlot, &closurePtr, sizeof(closurePtr));

    VM_ElevateClosureCapabilities((void *)closurePtr);

    VM_ElevateSecurityContext();

    return invokeAndDecode(L, callFn, closureSlot, outResult);
}

void *LuauRuntime_LoadPersistent(const std::string &source, const char *chunkname) {
    std::lock_guard<std::mutex> lock(gVMCallMutex);
    if (!VM_IsReady()) {
        VM_Log("LuauRuntime_LoadPersistent: VM not ready\n");
        return NULL;
    }
    void *L = VM_GetLuaState();

    std::vector<uint8_t> bytecode;
    if (!compileLuauSource(source, bytecode)) {
        VM_Log("LuauRuntime_LoadPersistent: compile failed: %s\n", (const char *)bytecode.data() + 1);
        return NULL;
    }

    VM_LuauLoadFn loadFn = VM_GetLuauLoadFn();
    VM_CallDispatchFn callFn = VM_GetCallDispatchFn();
    if (!loadFn || !callFn) {
        VM_Log("LuauRuntime_LoadPersistent: luau_load/call dispatch not resolved\n");
        return NULL;
    }

    int loadResult = loadFn(L, chunkname, bytecode.data(), bytecode.size(), 0);
    if (loadResult != 0) {
        VM_Log("LuauRuntime_LoadPersistent: load returned %d\n", loadResult);
        return NULL;
    }

    uintptr_t topBefore = 0;
    VM_SafeReadBytes((const uint8_t *)L + 0x58, &topBefore, sizeof(topBefore));
    uintptr_t closureSlot = topBefore - 0x10;

    uintptr_t closurePtr = 0;
    if (!VM_SafeReadBytes((const void *)closureSlot, &closurePtr, sizeof(closurePtr)) || !closurePtr) {
        VM_Log("LuauRuntime_LoadPersistent: couldn't read closure pointer\n");
        return NULL;
    }

    VM_ElevateClosureCapabilities((void *)closurePtr);

    VM_ElevateSecurityContext();

    std::string setupResult;
    if (!invokeAndDecode(L, callFn, closureSlot, setupResult)) {
        VM_Log("LuauRuntime_LoadPersistent: setup script errored: %s\n", setupResult.c_str());
        return NULL;
    }
    VM_Log("LuauRuntime_LoadPersistent: pinned closure %p, setup result: %s\n", (void *)closurePtr, setupResult.c_str());
    return (void *)closurePtr;
}

bool LuauRuntime_CallPersistent(void *handle, std::string &outResult) {
    std::lock_guard<std::mutex> lock(gVMCallMutex);
    if (!handle || !VM_IsReady()) return false;
    void *L = VM_GetLuaState();

    VM_CallDispatchFn callFn = VM_GetCallDispatchFn();
    if (!callFn) return false;

    uintptr_t closureSlot = 0;
    if (!VM_SafeReadBytes((const uint8_t *)L + 0x58, &closureSlot, sizeof(closureSlot))) return false;

    uint8_t tvalue[16] = {0};
    memcpy(tvalue, &handle, sizeof(handle));
    int32_t tag = 8;
    memcpy(tvalue + 12, &tag, sizeof(tag));
    if (!VM_SafeWriteBytes((void *)closureSlot, tvalue, sizeof(tvalue))) return false;

    uintptr_t newTop = closureSlot + 0x10;
    if (!VM_SafeWriteBytes((void *)((const uint8_t *)L + 0x58), &newTop, sizeof(newTop))) return false;

    VM_ElevateSecurityContext();

    return invokeAndDecode(L, callFn, closureSlot, outResult);
}

bool LuauRuntime_ExposeStudioService(std::string &outResult) {
    std::lock_guard<std::mutex> lock(gVMCallMutex);
    if (!VM_IsReady()) {
        outResult = "VM not ready";
        return false;
    }

    void *instancePtr = VM_FindStudioServiceInstance();
    if (!instancePtr) {
        outResult = "StudioService instance not found (no DataModel captured yet, or not present as a child)";
        return false;
    }

    void *L = VM_GetLuaState();

    static const char *src = "local svc = ...\nStudioService = svc\n";
    std::vector<uint8_t> bytecode;
    if (!compileLuauSource(src, bytecode)) {
        outResult = "compile failed";
        return false;
    }

    VM_LuauLoadFn loadFn = VM_GetLuauLoadFn();
    VM_LuaNewthreadFn newthreadFn = VM_GetLuaNewthreadFn();
    VM_TaskDeferFn deferFn = VM_GetTaskDeferFn();
    if (!loadFn || !newthreadFn || !deferFn) {
        outResult = "luau_load/lua_newthread/task_defer not resolved";
        return false;
    }

    uintptr_t topBefore = 0;
    VM_SafeReadBytes((const uint8_t *)L + 0x58, &topBefore, sizeof(topBefore));

    void *freshL = newthreadFn(L);
    if (!freshL) {
        outResult = "lua_newthread returned null";
        return false;
    }
    VM_SafeWriteBytes((void *)((const uint8_t *)L + 0x58), &topBefore, sizeof(topBefore));

    VM_ElevateThreadCapabilities(freshL);

    int loadResult = loadFn(freshL, "=ExposeStudioService", bytecode.data(), bytecode.size(), 0);
    if (loadResult != 0) {
        outResult = "load returned " + std::to_string(loadResult);
        return false;
    }

    uintptr_t freshTopBeforeArg = 0;
    VM_SafeReadBytes((const uint8_t *)freshL + 0x58, &freshTopBeforeArg, sizeof(freshTopBeforeArg));

    VM_PushInstance(freshL, instancePtr);

    uintptr_t freshTopAfterArg = 0;
    VM_SafeReadBytes((const uint8_t *)freshL + 0x58, &freshTopAfterArg, sizeof(freshTopAfterArg));
    if (freshTopAfterArg != freshTopBeforeArg + 0x10) {
        VM_Log("LuauRuntime_ExposeStudioService: unexpected stack delta after VM_PushInstance (top %#llx -> %#llx) - deferring without the argument\n",
               (unsigned long long)freshTopBeforeArg, (unsigned long long)freshTopAfterArg);

        VM_SafeWriteBytes((void *)((const uint8_t *)freshL + 0x58), &freshTopBeforeArg, sizeof(freshTopBeforeArg));
    }

    uint64_t deferResult = deferFn(freshL);
    VM_Log("LuauRuntime_ExposeStudioService: handed fresh thread to task_defer, result=0x%llx, instance=%p\n",
           (unsigned long long)deferResult, instancePtr);
    outResult = "<deferred>";
    return true;
}
