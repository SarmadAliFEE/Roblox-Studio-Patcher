#import <Foundation/Foundation.h>
#include <mach-o/dyld.h>
#include <mach-o/loader.h>
#include <mach/mach.h>
#include <mach/mach_vm.h>
#include <sys/mman.h>
#include <libkern/OSCacheControl.h>
#include <unistd.h>
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <stdarg.h>
#include <errno.h>
#include <setjmp.h>
#include <signal.h>
#include <pthread.h>
#include <ctype.h>
#include <time.h>
#include <stdlib.h>
#include <vector>
#include <string>
#include <mutex>

#include "vm_platform.h"
#include "luau_runtime.h"
#include "plugin_loader.h"
#include "discord_presence.h"

#define DISCORD_CLIENT_ID "1540119100318027776"

#pragma mark - Logging

static FILE *gLog = NULL;
static void logmsg(const char *fmt, ...) {
    if (!gLog) return;
    va_list args;
    va_start(args, fmt);
    vfprintf(gLog, fmt, args);
    va_end(args);
    fflush(gLog);
}

#pragma mark - Signal-safe memory probing

static thread_local sigjmp_buf gSafeReadJmpBuf;
static thread_local bool gInSafeRead = false;

static struct sigaction gOldSegvAction, gOldBusAction;

static void safeReadSignalHandler(int sig) {
    if (gInSafeRead) {
        gInSafeRead = false;
        siglongjmp(gSafeReadJmpBuf, 1);
    }
    struct sigaction *old = (sig == SIGBUS) ? &gOldBusAction : &gOldSegvAction;
    sigaction(sig, old, NULL);
    raise(sig);
}

static thread_local bool gThreadAltStackInstalled = false;
static void installAltSignalStackForCurrentThread(void) {
    if (gThreadAltStackInstalled) return;
    static thread_local unsigned char altStackBuf[SIGSTKSZ * 4];
    stack_t ss = {altStackBuf, 0, sizeof(altStackBuf)};
    sigaltstack(&ss, NULL);
    gThreadAltStackInstalled = true;
}

static bool isInCurrentStackRange(const void *ptr) {
    void *stackTop = pthread_get_stackaddr_np(pthread_self());
    size_t stackSize = pthread_get_stacksize_np(pthread_self());
    uintptr_t top = (uintptr_t)stackTop;
    uintptr_t bottom = (stackSize < top) ? (top - stackSize) : 0;
    uintptr_t margin = 0x200000;
    uintptr_t marginBottom = (bottom > margin) ? (bottom - margin) : 0;
    uintptr_t v = (uintptr_t)ptr;
    return v >= marginBottom && v <= top;
}

static bool gHandlerInstalled = false;
static void installSafeReadHandler(void) {
    installAltSignalStackForCurrentThread();
    if (gHandlerInstalled) return;
    struct sigaction newAction;
    memset(&newAction, 0, sizeof(newAction));
    newAction.sa_handler = safeReadSignalHandler;
    sigemptyset(&newAction.sa_mask);
    newAction.sa_flags = SA_ONSTACK;
    sigaction(SIGSEGV, &newAction, &gOldSegvAction);
    sigaction(SIGBUS, &newAction, &gOldBusAction);
    gHandlerInstalled = true;
}

static bool isAddressMappedWithProtection(const void *addr, size_t n, vm_prot_t requiredProt) {
    mach_vm_address_t regionAddr = (mach_vm_address_t)(uintptr_t)addr;
    mach_vm_size_t regionSize = 0;
    vm_region_basic_info_data_64_t info;
    mach_msg_type_number_t infoCount = VM_REGION_BASIC_INFO_COUNT_64;
    mach_port_t objectName = MACH_PORT_NULL;

    kern_return_t kr = mach_vm_region(mach_task_self(), &regionAddr, &regionSize,
                                       VM_REGION_BASIC_INFO_64, (vm_region_info_t)&info,
                                       &infoCount, &objectName);
    if (kr != KERN_SUCCESS) return false;

    uintptr_t regionStart = (uintptr_t)regionAddr;
    uintptr_t regionEnd = regionStart + (uintptr_t)regionSize;
    uintptr_t queryStart = (uintptr_t)addr;
    uintptr_t queryEnd = queryStart + n;
    // mach_vm_region returns the next mapped region at or after the
    // requested address when the exact address isn't itself mapped - the
    // query range actually landing inside what it returned still needs
    // confirming, not just that SOME region exists nearby.
    if (queryStart < regionStart || queryEnd > regionEnd) return false;
    return (info.protection & requiredProt) == requiredProt;
}

static bool safeReadBytes(const void *addr, void *out, size_t n) {
    if (isInCurrentStackRange(addr)) return false;
    if (!isAddressMappedWithProtection(addr, n, VM_PROT_READ)) return false;
    installAltSignalStackForCurrentThread();
    gInSafeRead = true;
    if (sigsetjmp(gSafeReadJmpBuf, 1) != 0) {
        gInSafeRead = false;
        return false;
    }
    memcpy(out, addr, n);
    gInSafeRead = false;
    return true;
}

static bool safeWriteBytes(void *addr, const void *in, size_t n) {
    if (isInCurrentStackRange(addr)) return false;
    if (!isAddressMappedWithProtection(addr, n, VM_PROT_WRITE)) return false;
    installAltSignalStackForCurrentThread();
    gInSafeRead = true;
    if (sigsetjmp(gSafeReadJmpBuf, 1) != 0) {
        gInSafeRead = false;
        return false;
    }
    memcpy(addr, in, n);
    gInSafeRead = false;
    return true;
}

static inline bool looksLikeHeapPointer(uintptr_t v) {
    return v >= 0x1000 && v <= 0x7fffffffffffULL;
}

#pragma mark - Mach-O image introspection

static const struct mach_header_64 *gImageHeader = NULL;
static intptr_t gImageSlide = 0;

static void findMainImage(void) {
    uint32_t count = _dyld_image_count();
    for (uint32_t i = 0; i < count; i++) {
        const char *name = _dyld_get_image_name(i);
        if (name && strstr(name, "RobloxStudio") != NULL && strstr(name, ".dylib") == NULL) {
            gImageHeader = (const struct mach_header_64 *)_dyld_get_image_header(i);
            gImageSlide = _dyld_get_image_vmaddr_slide(i);
            logmsg("main image: %s slide=0x%lx\n", name, (long)gImageSlide);
            return;
        }
    }
}

struct SegRange {
    uintptr_t start;
    uintptr_t end;
};

static std::vector<SegRange> getSegmentsByPrefix(const char *prefix) {
    std::vector<SegRange> out;
    if (!gImageHeader) return out;
    size_t prefixLen = strlen(prefix);
    const uint8_t *cmdPtr = (const uint8_t *)gImageHeader + sizeof(struct mach_header_64);
    for (uint32_t i = 0; i < gImageHeader->ncmds; i++) {
        const struct load_command *lc = (const struct load_command *)cmdPtr;
        if (lc->cmd == LC_SEGMENT_64) {
            const struct segment_command_64 *seg = (const struct segment_command_64 *)lc;
            if (strncmp(seg->segname, prefix, prefixLen) == 0 && seg->vmsize > 0) {
                uintptr_t start = (uintptr_t)seg->vmaddr + gImageSlide;
                out.push_back({start, start + (uintptr_t)seg->vmsize});
            }
        }
        cmdPtr += lc->cmdsize;
    }
    return out;
}

#pragma mark - Byte-pattern signature scanning

struct PatByte {
    bool wild;
    uint8_t val;
};

static std::vector<PatByte> parsePattern(const char *s) {
    std::vector<PatByte> out;
    const char *p = s;
    while (*p) {
        while (*p == ' ') p++;
        if (!*p) break;
        if (p[0] == '?') {
            out.push_back({true, 0});
            while (*p && *p != ' ') p++;
        } else {
            unsigned int v = 0;
            sscanf(p, "%2x", &v);
            out.push_back({false, (uint8_t)v});
            p += 2;
        }
    }
    return out;
}

static std::vector<uintptr_t> findPatternMatches(const std::vector<SegRange> &segs, const std::vector<PatByte> &pat) {
    std::vector<uintptr_t> hits;
    size_t patLen = pat.size();
    for (const auto &seg : segs) {
        const uint8_t *base = (const uint8_t *)seg.start;
        size_t len = seg.end - seg.start;
        if (len < patLen) continue;
        for (size_t i = 0; i + patLen <= len; i++) {
            bool ok = true;
            for (size_t j = 0; j < patLen; j++) {
                if (!pat[j].wild && base[i + j] != pat[j].val) { ok = false; break; }
            }
            if (ok) hits.push_back((uintptr_t)(base + i));
        }
    }
    return hits;
}

static uintptr_t findExactBytes(const uint8_t *needle, size_t needleLen, const std::vector<SegRange> &segs) {
    for (const auto &seg : segs) {
        size_t len = seg.end - seg.start;
        if (len < needleLen) continue;
        void *found = memmem((const void *)seg.start, len, needle, needleLen);
        if (found) return (uintptr_t)found;
    }
    return 0;
}

static std::vector<uintptr_t> scanForPointerMatches(uintptr_t target, const std::vector<SegRange> &segs) {
    std::vector<uintptr_t> hits;
    for (const auto &seg : segs) {
        uintptr_t p = seg.start & ~(uintptr_t)7;
        for (; p + 8 <= seg.end; p += 8) {
            if (*(const uintptr_t *)p == target) hits.push_back(p);
        }
    }
    return hits;
}

#pragma mark - RTTI-anchored primary-vtable resolution

static uintptr_t findPrimaryVtable(const char *rttiName, const std::vector<SegRange> &textSegs, const std::vector<SegRange> &dataSegs) {
    size_t nameLen = strlen(rttiName) + 1;
    uintptr_t nameAddr = findExactBytes((const uint8_t *)rttiName, nameLen, textSegs);
    if (!nameAddr) {
        logmsg("RTTI string not found: %s\n", rttiName);
        return 0;
    }

    for (uintptr_t nameRefAddr : scanForPointerMatches(nameAddr, dataSegs)) {
        uintptr_t typeinfoAddr = nameRefAddr - 8;
        for (uintptr_t tiRefAddr : scanForPointerMatches(typeinfoAddr, dataSegs)) {
            uintptr_t offsetToTopAddr = tiRefAddr - 8;
            intptr_t offsetToTop = *(const intptr_t *)offsetToTopAddr;
            if (offsetToTop != 0) continue;
            uintptr_t vtableFuncsStart = tiRefAddr + 8;
            logmsg("primary vtable for %s @ 0x%lx\n", rttiName, (unsigned long)vtableFuncsStart);
            return vtableFuncsStart;
        }
    }
    logmsg("no primary vtable found for %s\n", rttiName);
    return 0;
}

static int findSlotIndexForFunction(uintptr_t vtableBase, uintptr_t funcAddr, int maxSlots) {
    for (int i = 0; i < maxSlots; i++) {
        if (*(const uintptr_t *)(vtableBase + (uintptr_t)i * 8) == funcAddr) return i;
    }
    return -1;
}

static bool objectMatchesVtable(void *obj, uintptr_t vtable) {
    if (!obj || !vtable) return false;
    uintptr_t objVtable = 0;
    if (!safeReadBytes(obj, &objVtable, sizeof(objVtable))) return false;
    return objVtable == vtable;
}

static uintptr_t decodeArm64BLTarget(uintptr_t blInstrAddr) {
    uint32_t instr = 0;
    if (!safeReadBytes((const void *)blInstrAddr, &instr, sizeof(instr))) return 0;
    int32_t imm26 = instr & 0x03FFFFFF;
    if (imm26 & 0x02000000) imm26 |= 0xFC000000; // sign-extend bit 25 upward
    return blInstrAddr + ((intptr_t)imm26 << 2);
}

#pragma mark - Function signatures

static const char *STEP_SIGNATURE =
    "ff 43 03 d1 f6 57 0a a9 f4 4f 0b a9 fd 7b 0c a9 "
    "fd 03 03 91 f3 03 01 aa f4 03 00 aa 08 00 46 39 "
    "a8 00 00 37 88 c6 40 f9 08 01 1b 91 08 fd df 08 "
    "?? ?? ?? ?? 80 c6 40 f9 a8 03 01 d1 "
    "?? ?? ?? ?? ?? ?? ?? ??";

static const char *LUAU_LOAD_SIGNATURE =
    "fc 6f ba a9 fa 67 01 a9 f8 5f 02 a9 f6 57 03 a9 "
    "f4 4f 04 a9 fd 7b 05 a9 fd 43 01 91 ff 83 08 d1 "
    "f4 03 01 aa ?? ?? ?? ?? 08 f9 45 f9 08 01 40 f9 "
    "a8 03 1a f8 36 54 43 a9 a9 02 40 39 e1 03 0e a9 "
    "09 04 00 34 28 3d 00 51 3f 91 01 71 02 19 4d 3a";


static const char *LUAU_LOAD_WRAPPER_SIGNATURE =
    "ff 83 02 d1 fa 67 05 a9 f8 5f 06 a9 f6 57 07 a9 "
    "f4 4f 08 a9 fd 7b 09 a9 fd 43 02 91 f4 03 04 aa "
    "f5 03 03 aa f6 03 02 aa f7 03 01 aa f3 03 00 aa "
    "18 18 40 f9 19 23 45 a9 1f 01 19 eb c3 00 00 54";

static const char *CALL_DISPATCH_SIGNATURE =
    "f6 57 bd a9 f4 4f 01 a9 fd 7b 02 a9 fd 83 00 91 "
    "f4 03 02 aa f3 03 00 aa ?? ?? ?? ?? f5 03 00 aa "
    "60 02 00 35 68 1a 40 f9 08 95 42 f9 a8 02 00 b5 "
    "75 12 40 79 68 2e 40 f9 02 d1 34 cb ?? ?? ?? ??";


static const char *TASK_DEFER_SIGNATURE =
    "ff 83 03 d1 f8 5f 0a a9 f6 57 0b a9 f4 4f 0c a9 "
    "fd 7b 0d a9 fd 43 03 91 f3 03 00 aa ?? ?? ?? ?? "
    "?? ?? ?? ?? 08 01 00 37 ?? ?? ?? ??";

static const char *LUA_NEWTHREAD_SIGNATURE =
    "f4 4f be a9 fd 7b 01 a9 fd 43 00 91 f3 03 00 aa "
    "08 18 40 f9 08 25 45 a9 3f 01 08 eb 83 00 00 54 "
    "e0 03 13 aa 21 00 80 52 ?? ?? ?? ??";

static const char *CAN_ACCESS_RESTRICTED_SIGNATURE =
    "f6 57 bd a9 f4 4f 01 a9 fd 7b 02 a9 fd 83 00 91 "
    "f4 03 00 aa 15 0c 40 f9 ?? ?? ?? ?? f3 03 00 aa "
    "b5 42 46 39 35 01 00 b4 60 a2 42 a9 88 00 00 b4 "
    "61 0e 40 f9 00 01 3f d6 60 fe 02 a9 e8 03 20 2a "
    "1f 01 15 ea a1 01 00 54 94 ae 42 39 34 01 00 b4 "
    "60 a2 42 a9 88 00 00 b4 61 0e 40 f9 00 01 3f d6 "
    "60 fe 02 a9 e8 03 20 2a 1f 01 14 ea 61 00 00 54 "
    "20 00 80 52 02 00 00 14 00 00 80 52 fd 7b 42 a9 "
    "f4 4f 41 a9 f6 57 c3 a8 c0 03 5f d6";

#define CAN_ACCESS_RESTRICTED_BL_OFFSET 0x18

static const char *DATAMODEL_RTTI = "N3RBX9DataModelE";
static const char *SCRIPTCONTEXT_RTTI = "N3RBX13ScriptContextE";
static const char *RESTRICTED_STUDIOSERVICE_RTTI = "N3RBX24RESTRICTED_StudioServiceE";

static const char *JOB_CLASS_CANDIDATES[] = {
    "N3RBX12DataModelJobE",
    "N3RBX19GenericDataModelJobE",
    "N3RBX13TaskScheduler3JobE",
    "N3RBX9DataModel10GenericJobE",
    "N3RBX19ScriptContextFacets23WaitingHybridScriptsJobE",
};

#pragma mark - Live object discovery

#define STAGE_ITER_BUDGET 12

#define INNER_SEARCH_BUDGET 16

static void *findInstanceByVtable(void *root, int numFields, uintptr_t targetVtable, int *outOffset, int *ioResumeIndex) {
    if (!root) return NULL;
    int start = ioResumeIndex ? *ioResumeIndex : 0;
    int end = start + STAGE_ITER_BUDGET;
    if (end > numFields) end = numFields;

    for (int i = start; i < end; i++) {
        uintptr_t val = 0;
        if (!safeReadBytes((const uint8_t *)root + (uintptr_t)i * 8, &val, sizeof(val))) continue;
        if (!looksLikeHeapPointer(val)) continue;

        if (objectMatchesVtable((void *)val, targetVtable)) {
            if (outOffset) *outOffset = i * 8;
            if (ioResumeIndex) *ioResumeIndex = 0;
            return (void *)val;
        }

        int innerLimit = numFields < INNER_SEARCH_BUDGET ? numFields : INNER_SEARCH_BUDGET;
        for (int j = 0; j < innerLimit; j++) {
            uintptr_t innerVal = 0;
            if (!safeReadBytes((const uint8_t *)val + (uintptr_t)j * 8, &innerVal, sizeof(innerVal))) continue;
            if (!looksLikeHeapPointer(innerVal)) continue;
            if (objectMatchesVtable((void *)innerVal, targetVtable)) {
                if (outOffset) *outOffset = i * 8 * 10000 + j * 8;
                if (ioResumeIndex) *ioResumeIndex = 0;
                return (void *)innerVal;
            }
        }
    }
    if (ioResumeIndex) *ioResumeIndex = (end >= numFields) ? 0 : end;
    return NULL;
}

static void *findInstanceInVectorFields(void *root, int numFields, uintptr_t targetVtable, int *outFieldIndex, int *ioResumeIndex) {
    if (!root) return NULL;
    int start = ioResumeIndex ? *ioResumeIndex : 0;
    int end = start + STAGE_ITER_BUDGET;
    if (end > numFields - 1) end = numFields - 1;

    for (int i = start; i < end; i++) {
        uintptr_t base = (uintptr_t)root + (uintptr_t)i * 8;
        uintptr_t begin = 0, elemEnd = 0;
        if (!safeReadBytes((const void *)base, &begin, sizeof(begin))) continue;
        if (!safeReadBytes((const void *)(base + 8), &elemEnd, sizeof(elemEnd))) continue;
        if (begin < 0x1000 || elemEnd < begin) continue;
        uintptr_t span = elemEnd - begin;
        if (span == 0 || span > 0x40000) continue;

        for (int stride = 8; stride <= 16; stride += 8) {
            if (span % stride != 0) continue;
            int count = (int)(span / stride);
            if (count > 4096) continue;
            for (int e = 0; e < count; e++) {
                uintptr_t elemVal = 0;
                if (!safeReadBytes((const void *)(begin + (uintptr_t)e * stride), &elemVal, sizeof(elemVal))) continue;
                if (!looksLikeHeapPointer(elemVal)) continue;
                if (objectMatchesVtable((void *)elemVal, targetVtable)) {
                    if (outFieldIndex) *outFieldIndex = i * 8;
                    if (ioResumeIndex) *ioResumeIndex = 0;
                    return (void *)elemVal;
                }
            }
        }
    }
    if (ioResumeIndex) *ioResumeIndex = (end >= numFields - 1) ? 0 : end;
    return NULL;
}

static bool looksLikeLuaState(void *candidate, void *expectedScriptContext) {
    if (!candidate) return false;
    uintptr_t limitPtr = 0, topPtr = 0;
    if (!safeReadBytes((const uint8_t *)candidate + 0x50, &limitPtr, sizeof(limitPtr))) return false;
    if (!safeReadBytes((const uint8_t *)candidate + 0x58, &topPtr, sizeof(topPtr))) return false;
    if (!looksLikeHeapPointer(limitPtr) || !looksLikeHeapPointer(topPtr)) return false;

    uintptr_t limitDeref = 0;
    if (!safeReadBytes((const void *)limitPtr, &limitDeref, sizeof(limitDeref))) return false;
    intptr_t delta = (intptr_t)limitDeref - (intptr_t)topPtr;
    if (delta < -0x100000 || delta > 0x100000) return false;

    uintptr_t globalPtr = 0;
    if (!safeReadBytes((const uint8_t *)candidate + 0x30, &globalPtr, sizeof(globalPtr))) return false;
    if (!looksLikeHeapPointer(globalPtr)) return false;

    int32_t depthCounter = 0;
    if (!safeReadBytes((const uint8_t *)globalPtr + 0x4980, &depthCounter, sizeof(depthCounter))) return false;
    if (depthCounter < 0 || depthCounter > 10000) return false;

    if (expectedScriptContext) {
        uintptr_t extraSpace = 0, shared = 0, boundCtx = 0;
        if (!safeReadBytes((const uint8_t *)candidate + 0x78, &extraSpace, sizeof(extraSpace))) return false;
        if (!extraSpace || !looksLikeHeapPointer(extraSpace)) return false;
        if (!safeReadBytes((const uint8_t *)extraSpace + 0x18, &shared, sizeof(shared))) return false;
        if (!shared || !looksLikeHeapPointer(shared)) return false;
        if (!safeReadBytes((const uint8_t *)shared + 0x18, &boundCtx, sizeof(boundCtx))) return false;
        if ((void *)boundCtx != expectedScriptContext) return false;
    }

    return true;
}

static void *findLuaStateNear(void *root, int numFields, int *ioResumeIndex) {
    if (!root) return NULL;
    int start = ioResumeIndex ? *ioResumeIndex : 0;
    int end = start + STAGE_ITER_BUDGET;
    if (end > numFields) end = numFields;

    int innerLimit = numFields < INNER_SEARCH_BUDGET ? numFields : INNER_SEARCH_BUDGET;

    for (int i = start; i < end; i++) {
        uintptr_t val = 0;
        if (!safeReadBytes((const uint8_t *)root + (uintptr_t)i * 8, &val, sizeof(val))) continue;
        if (!looksLikeHeapPointer(val)) continue;
        if (looksLikeLuaState((void *)val, root)) {
            if (ioResumeIndex) *ioResumeIndex = 0;
            return (void *)val;
        }

        for (int j = 0; j < innerLimit; j++) {
            uintptr_t innerVal = 0;
            if (!safeReadBytes((const uint8_t *)val + (uintptr_t)j * 8, &innerVal, sizeof(innerVal))) continue;
            if (!looksLikeHeapPointer(innerVal)) continue;
            if (looksLikeLuaState((void *)innerVal, root)) {
                if (ioResumeIndex) *ioResumeIndex = 0;
                return (void *)innerVal;
            }
        }
    }
    if (ioResumeIndex) *ioResumeIndex = (end >= numFields) ? 0 : end;
    return NULL;
}

#pragma mark - Capability elevation

#define ELEVATED_CAPABILITIES_VALUE 0xFFFFFFFFFFFFFFFFULL

static bool elevateThreadCapabilities(void *L) {
    if (!L) return false;
    void *extraSpace = NULL;
    if (!safeReadBytes((const uint8_t *)L + 0x78, &extraSpace, sizeof(extraSpace)) || !extraSpace) {
        logmsg("diag: elevateThreadCapabilities: couldn't read ExtraSpace for L=%p\n", L);
        return false;
    }

    uint64_t oldFlat = 0;
    safeReadBytes((const uint8_t *)extraSpace + 0x40, &oldFlat, sizeof(oldFlat));
    uint64_t desiredFlat = ELEVATED_CAPABILITIES_VALUE;
    bool wroteFlat = safeWriteBytes((void *)((uint8_t *)extraSpace + 0x40), &desiredFlat, sizeof(desiredFlat));
    logmsg("diag: elevateThreadCapabilities L=%p ExtraSpace=%p oldFlat=%#llx wroteFlat=%d\n",
           L, extraSpace, (unsigned long long)oldFlat, (int)wroteFlat);
    return wroteFlat;
}

void VM_ElevateThreadCapabilities(void *L) {
    elevateThreadCapabilities(L);
}

typedef void *(*SecurityContextCurrentFn)(void);
static void *gSecurityContextCurrentFn = NULL;

static void *findSecurityContextCurrentFn(void) {
    if (gSecurityContextCurrentFn) return gSecurityContextCurrentFn;

    std::vector<SegRange> textSegs = getSegmentsByPrefix("__TEXT");
    std::vector<uintptr_t> hits = findPatternMatches(textSegs, parsePattern(CAN_ACCESS_RESTRICTED_SIGNATURE));
    logmsg("diag: canAccessRestrictedInstanceImpl signature matches: %zu\n", hits.size());
    if (hits.empty()) {
        logmsg("canAccessRestrictedInstanceImpl not found - Roblox Studio likely updated and this signature needs refreshing\n");
        return NULL;
    }
    if (hits.size() > 1) {
        logmsg("WARNING: canAccessRestrictedInstanceImpl signature ambiguous (%zu matches), using first\n", hits.size());
    }

    uintptr_t blAddr = hits[0] + CAN_ACCESS_RESTRICTED_BL_OFFSET;
    uintptr_t target = decodeArm64BLTarget(blAddr);
    if (!target) {
        logmsg("diag: findSecurityContextCurrentFn: couldn't decode bl @ 0x%lx\n", (unsigned long)blAddr);
        return NULL;
    }
    gSecurityContextCurrentFn = (void *)target;
    logmsg("diag: Security::Context::current() resolved @ 0x%lx (via canAccessRestrictedInstanceImpl @ 0x%lx)\n",
           (unsigned long)target, (unsigned long)hits[0]);
    return gSecurityContextCurrentFn;
}

void VM_ElevateSecurityContext(void) {
    SecurityContextCurrentFn currentFn = (SecurityContextCurrentFn)findSecurityContextCurrentFn();
    if (!currentFn) return;

    void *context = currentFn();
    if (!context) {
        logmsg("diag: VM_ElevateSecurityContext: Security::Context::current() returned NULL\n");
        return;
    }

    uint64_t oldCached = 0;
    safeReadBytes((const uint8_t *)context + 0x28, &oldCached, sizeof(oldCached));
    uint64_t newCached = ELEVATED_CAPABILITIES_VALUE;
    bool wroteCached = safeWriteBytes((uint8_t *)context + 0x28, &newCached, sizeof(newCached));

    void *oldLazyFn = NULL;
    safeReadBytes((const uint8_t *)context + 0x30, &oldLazyFn, sizeof(oldLazyFn));
    void *clearedLazyFn = NULL;
    bool wroteLazyFn = safeWriteBytes((uint8_t *)context + 0x30, &clearedLazyFn, sizeof(clearedLazyFn));

    logmsg("diag: VM_ElevateSecurityContext context=%p oldCached=%#llx wroteCached=%d oldLazyFn=%p wroteLazyFn=%d pthread_self=%p\n",
           context, (unsigned long long)oldCached, (int)wroteCached, oldLazyFn, (int)wroteLazyFn, (void *)pthread_self());
}


#define CLOSURE_OFF_ISC 0x3
#define CLOSURE_OFF_L_P 0x18 // struct Proto *l.p (union with c.f/... for C closures) - confirmed via the live closure constructor, FUN_1068eb2e0

#define PROTO_OFF_P 0x10     // struct Proto **p - confirmed via the bytecode deserializer, FUN_106906700
#define PROTO_OFF_SIZEP 0x8c // int sizep - confirmed the same way

static void dumpProtoRecursive(void *proto, int depth) {
    if (!proto || depth > 8) return;

    uint8_t raw[0xE0] = {0};
    if (!safeReadBytes(proto, raw, sizeof(raw))) {
        logmsg("diag: dumpProtoRecursive: couldn't read Proto bytes @ %p (depth=%d)\n", proto, depth);
        return;
    }
    char hexbuf[0xE0 * 3 + 1];
    for (size_t i = 0; i < sizeof(raw); i++) snprintf(hexbuf + i * 3, 4, "%02x ", raw[i]);
    logmsg("diag: dumpProtoRecursive depth=%d proto=%p bytes[0x00-0xE0]: %s\n", depth, proto, hexbuf);

    int32_t sizep = 0;
    void *pArray = NULL;
    if (!safeReadBytes((const uint8_t *)proto + PROTO_OFF_SIZEP, &sizep, sizeof(sizep))) return;
    if (sizep <= 0 || sizep > 64) return;
    if (!safeReadBytes((const uint8_t *)proto + PROTO_OFF_P, &pArray, sizeof(pArray)) || !pArray) return;

    for (int32_t i = 0; i < sizep; i++) {
        void *childProto = NULL;
        if (!safeReadBytes((const uint8_t *)pArray + (size_t)i * sizeof(void *), &childProto, sizeof(childProto))) continue;
        dumpProtoRecursive(childProto, depth + 1);
    }
}

#define PROTO_USERDATA_CANDIDATE_OFF 0x70
void VM_TestProtoSentinelWrite(void *closurePtr) {
    if (!closurePtr) return;
    uint8_t isC = 0;
    if (!safeReadBytes((const uint8_t *)closurePtr + CLOSURE_OFF_ISC, &isC, sizeof(isC)) || isC) return;
    void *proto = NULL;
    if (!safeReadBytes((const uint8_t *)closurePtr + CLOSURE_OFF_L_P, &proto, sizeof(proto)) || !proto) return;

    uint64_t before = 0;
    safeReadBytes((const uint8_t *)proto + PROTO_USERDATA_CANDIDATE_OFF, &before, sizeof(before));
    uint64_t sentinel = 0x4141414141414141ULL;
    bool wrote = safeWriteBytes((uint8_t *)proto + PROTO_USERDATA_CANDIDATE_OFF, &sentinel, sizeof(sentinel));
    uint64_t after = 0;
    safeReadBytes((const uint8_t *)proto + PROTO_USERDATA_CANDIDATE_OFF, &after, sizeof(after));
    logmsg("diag: VM_TestProtoSentinelWrite proto=%p off=0x%x before=%#llx wrote=%d after=%#llx\n",
           proto, PROTO_USERDATA_CANDIDATE_OFF, (unsigned long long)before, (int)wrote, (unsigned long long)after);
}

void VM_DumpProtoBytes(void *closurePtr) {
    if (!closurePtr) return;

    uint8_t isC = 0;
    if (!safeReadBytes((const uint8_t *)closurePtr + CLOSURE_OFF_ISC, &isC, sizeof(isC))) {
        logmsg("diag: VM_DumpProtoBytes: couldn't read isC for closure=%p\n", closurePtr);
        return;
    }
    if (isC) {
        logmsg("diag: VM_DumpProtoBytes: closure=%p is a C closure, no Proto\n", closurePtr);
        return;
    }

    void *proto = NULL;
    if (!safeReadBytes((const uint8_t *)closurePtr + CLOSURE_OFF_L_P, &proto, sizeof(proto)) || !proto) {
        logmsg("diag: VM_DumpProtoBytes: couldn't read Proto for closure=%p\n", closurePtr);
        return;
    }

    dumpProtoRecursive(proto, 0);
}

#define PROTO_OFF_CAPABILITY_OVERRIDE 0x60

static void elevateProtoRecursive(void *proto, void *capsValuePtr, int depth) {
    if (!proto || depth > 8) return;

    safeWriteBytes((uint8_t *)proto + PROTO_OFF_CAPABILITY_OVERRIDE, &capsValuePtr, sizeof(capsValuePtr));

    int32_t sizep = 0;
    void *pArray = NULL;
    if (!safeReadBytes((const uint8_t *)proto + PROTO_OFF_SIZEP, &sizep, sizeof(sizep))) return;
    if (sizep <= 0 || sizep > 64) return;
    if (!safeReadBytes((const uint8_t *)proto + PROTO_OFF_P, &pArray, sizeof(pArray)) || !pArray) return;

    for (int32_t i = 0; i < sizep; i++) {
        void *childProto = NULL;
        if (!safeReadBytes((const uint8_t *)pArray + (size_t)i * sizeof(void *), &childProto, sizeof(childProto))) continue;
        elevateProtoRecursive(childProto, capsValuePtr, depth + 1);
    }
}

static uint64_t *gCapabilityOverrideValue = NULL;

void VM_ElevateClosureCapabilities(void *closurePtr) {
    if (!closurePtr) return;
    uint8_t isC = 0;
    if (!safeReadBytes((const uint8_t *)closurePtr + CLOSURE_OFF_ISC, &isC, sizeof(isC)) || isC) return;
    void *proto = NULL;
    if (!safeReadBytes((const uint8_t *)closurePtr + CLOSURE_OFF_L_P, &proto, sizeof(proto)) || !proto) return;

    if (!gCapabilityOverrideValue) {
        gCapabilityOverrideValue = (uint64_t *)malloc(sizeof(uint64_t));
        *gCapabilityOverrideValue = ELEVATED_CAPABILITIES_VALUE;
    }

    elevateProtoRecursive(proto, gCapabilityOverrideValue, 0);
    logmsg("diag: VM_ElevateClosureCapabilities: stamped closure=%p proto=%p override=%p value=%#llx\n",
           closurePtr, proto, (void *)gCapabilityOverrideValue, (unsigned long long)*gCapabilityOverrideValue);
}

static void *gLuauLoadWrapperFn = NULL;
static void *gCallDispatchFn = NULL;
static void *gTaskDeferFn = NULL;
static void *gLuaNewthreadFn = NULL;

#pragma mark - Vtable patching

static bool patchVtableSlot(uintptr_t slotAddr, void *newFn) {
    long pageSize = sysconf(_SC_PAGESIZE);
    uintptr_t pageStart = slotAddr & ~(uintptr_t)(pageSize - 1);
    if (mprotect((void *)pageStart, pageSize, PROT_READ | PROT_WRITE) != 0) {
        logmsg("mprotect RW on vtable page failed: %s\n", strerror(errno));
        return false;
    }
    *(void **)slotAddr = newFn;
    if (mprotect((void *)pageStart, pageSize, PROT_READ) != 0) {
        logmsg("mprotect restore R on vtable page failed (non-fatal, page stays writable): %s\n", strerror(errno));
    }
    return true;
}

#pragma mark - DataModelJob::step hook + discovery state machine

typedef void *(*StepFnType)(void *, void *);

static void *gOriginalStepFn = NULL;
static void *gCapturedDataModelJob = NULL;
static void *gCapturedDataModel = NULL;
static void *gCapturedScriptContext = NULL;
static void *gCapturedLuaState = NULL;
static uintptr_t gDataModelVtable = 0;
static uintptr_t gScriptContextVtable = 0;
static uintptr_t gWaitingHybridScriptsJobVtable = 0;

static void *gAlternateJobCandidate = NULL;

#pragma mark - RESTRICTED_ service discovery + native Instance push

static void *gStudioServiceInstance = NULL;
static uintptr_t gStudioServiceVtable = 0;

#define PUSH_INSTANCE_FN_STATIC_ADDR 0x10341b7c0ULL
typedef void (*PushInstanceFn)(void *L, void **instancePtrSlot);

static void *findStudioServiceInstance(void) {
    if (gStudioServiceInstance) return gStudioServiceInstance;
    if (!gImageSlide || !gCapturedDataModel) return NULL;

    if (!gStudioServiceVtable) {
        std::vector<SegRange> textSegs = getSegmentsByPrefix("__TEXT");
        std::vector<SegRange> dataSegs = getSegmentsByPrefix("__DATA");
        gStudioServiceVtable = findPrimaryVtable(RESTRICTED_STUDIOSERVICE_RTTI, textSegs, dataSegs);
        if (!gStudioServiceVtable) return NULL;
    }

    int outFieldIndex = 0;
    int resumeIdx = 0;
    for (int guard = 0; guard < 64; guard++) {
        void *found = findInstanceInVectorFields(gCapturedDataModel, 0x60, gStudioServiceVtable, &outFieldIndex, &resumeIdx);
        if (found) {
            gStudioServiceInstance = found;
            logmsg("diag: findStudioServiceInstance: found @ %p (field offset 0x%x)\n", found, outFieldIndex);
            return found;
        }
        if (resumeIdx == 0) break; // scan exhausted, nothing found
    }
    logmsg("diag: findStudioServiceInstance: not found in DataModel's children\n");
    return NULL;
}

void *VM_FindStudioServiceInstance(void) {
    return findStudioServiceInstance();
}

void VM_PushInstance(void *L, void *instancePtr) {
    if (!gImageSlide || !instancePtr) return;
    PushInstanceFn pushFn = (PushInstanceFn)(gImageSlide + PUSH_INSTANCE_FN_STATIC_ADDR);
    pushFn(L, &instancePtr);
}

enum SearchStage {
    STAGE_FIND_DATAMODEL = 0,
    STAGE_FIND_SCRIPTCONTEXT_FLAT,
    STAGE_FIND_LUA_STATE,
    STAGE_READY,
};
static SearchStage gSearchStage = STAGE_FIND_DATAMODEL;

static uint64_t monotonicMillis(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000 + (uint64_t)ts.tv_nsec / 1000000;
}

#define SEARCH_RETRY_MS 100
#define REVALIDATE_MS 5000

#define REVALIDATE_FAILURE_THRESHOLD 3

#define SWITCH_CHECK_MS 500
#define VM_TOUCH_GRACE_MS 4000
static uint64_t gLastSearchAttemptMs = 0;
static uint64_t gLastRevalidateMs = 0;
static int gConsecutiveRevalidateFailures = 0;
static uint64_t gLastSwitchCheckMs = 0;

#define CAPTURED_JOB_STALE_MS 1500
static uint64_t gLastCapturedJobStepMs = 0;
static uint64_t gReadySinceMs = 0;
static bool gElevatedThisCapture = false;

static void writeCaptureStatus(void *self) {
    FILE *f = fopen("/tmp/studio_patcher_datamodeljob_captured.txt", "w");
    if (!f) return;
    fprintf(f, "self=%p\nDataModel=%p\nScriptContext=%p\nlua_State=%p\n",
            self, gCapturedDataModel, gCapturedScriptContext, gCapturedLuaState);
    fclose(f);
}

static int gDataModelResumeIdx = 0;
static int gLuaStateResumeIdx = 0;

static void runOneSearchStage(void *self) {
    int foundOffset = -1;
    switch (gSearchStage) {
        case STAGE_FIND_DATAMODEL:
            if (!gDataModelVtable) { gSearchStage = STAGE_READY; break; }
            gCapturedDataModel = findInstanceByVtable(self, 0x40, gDataModelVtable, &foundOffset, &gDataModelResumeIdx);
            if (gCapturedDataModel) {
                logmsg("stage DataModel: found @ %p\n", gCapturedDataModel);
                gSearchStage = STAGE_FIND_SCRIPTCONTEXT_FLAT;
            }
            break;

        case STAGE_FIND_SCRIPTCONTEXT_FLAT: {
            if (!gWaitingHybridScriptsJobVtable || !objectMatchesVtable(self, gWaitingHybridScriptsJobVtable)) {
                break;
            }

            void *sc = NULL;
            if (gScriptContextVtable) {
                for (uintptr_t off = 0; off < 0x400; off += 8) {
                    void *candidate = NULL;
                    if (!safeReadBytes((const uint8_t *)self + off, &candidate, sizeof(candidate))) continue;
                    if (objectMatchesVtable(candidate, gScriptContextVtable)) {
                        sc = candidate;
                        logmsg("diag: STAGE_FIND_SCRIPTCONTEXT_FLAT: ScriptContext vtable match @ self+0x%lx\n", (unsigned long)off);
                        break;
                    }
                }
            }
            if (sc) {
                gCapturedScriptContext = sc;
                logmsg("stage ScriptContext (direct, WaitingHybridScriptsJob): found @ %p\n", gCapturedScriptContext);

                void *dmPlus110 = NULL;
                safeReadBytes((const uint8_t *)gCapturedDataModel + 0x110, &dmPlus110, sizeof(dmPlus110));
                uint64_t ownerThread658 = 0;
                bool ownerOk = safeReadBytes((const uint8_t *)gCapturedScriptContext + 0x658, &ownerThread658, sizeof(ownerThread658));
                uint8_t flagAt4f5 = 0;
                bool flagOk = safeReadBytes((const uint8_t *)gCapturedScriptContext + 0x4f5, &flagAt4f5, sizeof(flagAt4f5));
                logmsg("diag: DataModel+0x110=%p (== ScriptContext? %d) sc+0x658=0x%llx (ok=%d) sc+0x4f5=0x%02x (ok=%d) calling pthread_self=%p\n",
                       dmPlus110, (int)(dmPlus110 == gCapturedScriptContext),
                       (unsigned long long)ownerThread658, (int)ownerOk,
                       (unsigned)flagAt4f5, (int)flagOk, (void *)pthread_self());

                gSearchStage = STAGE_FIND_LUA_STATE;
            }
            break;
        }

        case STAGE_FIND_LUA_STATE:
            gCapturedLuaState = findLuaStateNear(gCapturedScriptContext, 0x100, &gLuaStateResumeIdx);
            if (gCapturedLuaState) {
                logmsg("stage lua_State: found @ %p\n", gCapturedLuaState);
                gSearchStage = STAGE_READY;
                gReadySinceMs = monotonicMillis();
            }
            break;

        case STAGE_READY:
        default:
            break;
    }
    writeCaptureStatus(self);
}

static bool revalidateCapturedState(void) {
    if (!gCapturedDataModel || !gCapturedScriptContext || !gCapturedLuaState) return false;
    if (gDataModelVtable && !objectMatchesVtable(gCapturedDataModel, gDataModelVtable)) return false;
    if (gScriptContextVtable && !objectMatchesVtable(gCapturedScriptContext, gScriptContextVtable)) return false;
    return true;
}

static bool gCurrentJobIsGood = false;

#define FAILED_JOB_COOLDOWN_MS 5000
static void *gLastFailedJob = NULL;
static uint64_t gLastFailedJobMs = 0;

static void resetCapturedState(void) {
    logmsg("captured state no longer valid (place likely changed) - restarting discovery\n");
    gCapturedDataModelJob = NULL;
    gCapturedDataModel = NULL;
    gCapturedScriptContext = NULL;
    gCapturedLuaState = NULL;
    gSearchStage = STAGE_FIND_DATAMODEL;
    gDataModelResumeIdx = 0;
    gLuaStateResumeIdx = 0;
    gCurrentJobIsGood = false;
    gConsecutiveRevalidateFailures = 0;
    gLastCapturedJobStepMs = 0;
    gElevatedThisCapture = false;
}

#define DATAMODEL_GAME_STATE_TYPE_OFFSET 0x4f0
#define STUDIO_GAME_STATE_TYPE_EDIT 0

static bool currentDataModelLooksEmpty(void) {
    int32_t gameStateType = -1;
    bool ok = safeReadBytes((const uint8_t *)gCapturedDataModel + DATAMODEL_GAME_STATE_TYPE_OFFSET,
                             &gameStateType, sizeof(gameStateType));
    logmsg("currentDataModelLooksEmpty: DataModel=%p read_ok=%d gameStateType=%d\n",
           gCapturedDataModel, (int)ok, gameStateType);
    if (!ok) return false;
    return gameStateType != STUDIO_GAME_STATE_TYPE_EDIT;
}

static bool jobIsHybrid(void *job) {
    return gWaitingHybridScriptsJobVtable && objectMatchesVtable(job, gWaitingHybridScriptsJobVtable);
}

static void maybeSwitchToAlternateJob(bool forceStale) {
    if (gCurrentJobIsGood && !forceStale) return;

    bool currentIsHybrid = jobIsHybrid(gCapturedDataModelJob);
    bool currentIsEdit = gCapturedDataModel && !currentDataModelLooksEmpty();
    if (!forceStale && currentIsHybrid && currentIsEdit) {
        gCurrentJobIsGood = true;
        return;
    }

    if (!gAlternateJobCandidate) return;
    if (gAlternateJobCandidate == gLastFailedJob &&
        monotonicMillis() - gLastFailedJobMs < FAILED_JOB_COOLDOWN_MS) {
        return;
    }
    bool alternateIsHybrid = jobIsHybrid(gAlternateJobCandidate);

    bool shouldSwitch = false;
    if (forceStale) {
        shouldSwitch = true;
    } else if (!currentIsHybrid && alternateIsHybrid) {
        shouldSwitch = true;
    } else if (currentIsHybrid && !currentIsEdit && alternateIsHybrid) {
        shouldSwitch = true;
    }
    if (!shouldSwitch) return;

    void *next = gAlternateJobCandidate;
    logmsg("switching capture to job %p (currentIsHybrid=%d currentIsEdit=%d alternateIsHybrid=%d forceStale=%d)\n",
           next, (int)currentIsHybrid, (int)currentIsEdit, (int)alternateIsHybrid, (int)forceStale);
    resetCapturedState();
    gCapturedDataModelJob = next;
    gAlternateJobCandidate = NULL;
}

#define ALT_TRACK_SLOTS 16
#define ALT_PLAY_RECENT_MS 3000
#define ALT_DISCOVERY_THROTTLE_MS 200
static uint64_t gLastAltDiscoveryMs = 0;
struct AltJobTrack {
    void *job;
    void *dataModel;
    int32_t gameStateType;
    uint64_t lastSeenMs;
    int resumeIdx;
};
static AltJobTrack gAltTrack[ALT_TRACK_SLOTS] = {};

static void trackAlternateJob(void *self) {
    uint64_t now = monotonicMillis();
    int slot = -1;
    for (int i = 0; i < ALT_TRACK_SLOTS; i++) {
        if (gAltTrack[i].job == self) { slot = i; break; }
    }

    if (slot < 0) {
        // Slots are never explicitly freed (a job's own death is never
        // observed directly), so a long session eventually fills all 16 -
        // evict the least-recently-seen entry rather than permanently
        // refusing to track anything new past that point.
        uint64_t oldestSeenMs = UINT64_MAX;
        for (int i = 0; i < ALT_TRACK_SLOTS; i++) {
            if (gAltTrack[i].job == NULL) { slot = i; break; }
            if (gAltTrack[i].lastSeenMs < oldestSeenMs) {
                oldestSeenMs = gAltTrack[i].lastSeenMs;
                slot = i;
            }
        }
        logmsg("alternate job seen = %p is_main_thread=%d hybrid=%d\n",
               self, (int)pthread_main_np(), (int)jobIsHybrid(self));
        gAltTrack[slot].job = self;
        gAltTrack[slot].dataModel = NULL;
        gAltTrack[slot].gameStateType = -1;
        gAltTrack[slot].resumeIdx = 0;
    }
    gAltTrack[slot].lastSeenMs = now;

    if (gAltTrack[slot].dataModel) {
        safeReadBytes((const uint8_t *)gAltTrack[slot].dataModel + DATAMODEL_GAME_STATE_TYPE_OFFSET,
                       &gAltTrack[slot].gameStateType, sizeof(gAltTrack[slot].gameStateType));
        return;
    }

    if (now - gLastAltDiscoveryMs < ALT_DISCOVERY_THROTTLE_MS) return;
    gLastAltDiscoveryMs = now;

    int altOffset = -1;
    void *altDataModel = findInstanceByVtable(self, 0x40, gDataModelVtable, &altOffset, &gAltTrack[slot].resumeIdx);
    if (altDataModel) {
        gAltTrack[slot].dataModel = altDataModel;
        safeReadBytes((const uint8_t *)altDataModel + DATAMODEL_GAME_STATE_TYPE_OFFSET,
                       &gAltTrack[slot].gameStateType, sizeof(gAltTrack[slot].gameStateType));
        logmsg("diag: alternate job %p -> DataModel=%p gameStateType=%d\n",
               self, altDataModel, gAltTrack[slot].gameStateType);
    }
}

static bool isPlayTestGameStateType(int32_t t) {
    return t != STUDIO_GAME_STATE_TYPE_EDIT && t != 3 && t >= 0;
}

#pragma mark - vm_platform.h implementation

static bool capturedStateStillPlausible(void) {
    if (!gCapturedDataModel || !gCapturedScriptContext) return false;
    if (gDataModelVtable && !objectMatchesVtable(gCapturedDataModel, gDataModelVtable)) return false;
    if (gScriptContextVtable && !objectMatchesVtable(gCapturedScriptContext, gScriptContextVtable)) return false;
    return true;
}

bool VM_IsReady(void) {
    return gCapturedLuaState != NULL && capturedStateStillPlausible();
}

bool VM_IsPlayTestActive(void) {
    uint64_t now = monotonicMillis();
    for (int i = 0; i < ALT_TRACK_SLOTS; i++) {
        if (gAltTrack[i].job && isPlayTestGameStateType(gAltTrack[i].gameStateType) &&
            now - gAltTrack[i].lastSeenMs < ALT_PLAY_RECENT_MS) {
            return true;
        }
    }
    return false;
}

void *VM_GetLuaState(void) {
    if (!capturedStateStillPlausible()) return NULL;
    return gCapturedLuaState;
}

void VM_InvalidateCapturedState(void) {
    logmsg("diag: VM_InvalidateCapturedState: a real call came back structurally impossible, dropping capture\n");
    gLastFailedJob = gCapturedDataModelJob;
    gLastFailedJobMs = monotonicMillis();
    resetCapturedState();
}

VM_LuauLoadFn VM_GetLuauLoadFn(void) {
    return (VM_LuauLoadFn)gLuauLoadWrapperFn;
}

VM_CallDispatchFn VM_GetCallDispatchFn(void) {
    return (VM_CallDispatchFn)gCallDispatchFn;
}

VM_TaskDeferFn VM_GetTaskDeferFn(void) {
    return (VM_TaskDeferFn)gTaskDeferFn;
}

VM_LuaNewthreadFn VM_GetLuaNewthreadFn(void) {
    return (VM_LuaNewthreadFn)gLuaNewthreadFn;
}

bool VM_SafeReadBytes(const void *addr, void *out, size_t n) {
    return safeReadBytes(addr, out, n);
}

bool VM_SafeWriteBytes(void *addr, const void *in, size_t n) {
    return safeWriteBytes(addr, in, n);
}

void VM_Log(const char *fmt, ...) {
    if (!gLog) return;
    va_list args;
    va_start(args, fmt);
    vfprintf(gLog, fmt, args);
    va_end(args);
    fflush(gLog);
}

static std::mutex gHookMutex;

extern "C" void *hookedStep(void *self, void *stats) {
    {
        std::lock_guard<std::mutex> lock(gHookMutex);
        if (!gCapturedDataModelJob) {
            gCapturedDataModelJob = self;
            installSafeReadHandler();
            logmsg("captured first DataModelJob instance = %p is_main_thread=%d\n", self, (int)pthread_main_np());
            runOneSearchStage(self);
        } else if (self == gCapturedDataModelJob) {
            uint64_t now = monotonicMillis();
            gLastCapturedJobStepMs = now;

            if (gSearchStage != STAGE_READY) {
                if (now - gLastSearchAttemptMs >= SEARCH_RETRY_MS) {
                    gLastSearchAttemptMs = now;
                    runOneSearchStage(self);
                }
                if (now - gLastSwitchCheckMs >= SWITCH_CHECK_MS) {
                    gLastSwitchCheckMs = now;
                    maybeSwitchToAlternateJob(false);
                }
            } else {
                if (now - gLastRevalidateMs >= REVALIDATE_MS) {
                    gLastRevalidateMs = now;
                    if (!revalidateCapturedState()) {
                        gConsecutiveRevalidateFailures++;
                        logmsg("diag: revalidateCapturedState failed (%d/%d consecutive)\n",
                               gConsecutiveRevalidateFailures, REVALIDATE_FAILURE_THRESHOLD);
                        if (gConsecutiveRevalidateFailures >= REVALIDATE_FAILURE_THRESHOLD) {
                            gLastFailedJob = gCapturedDataModelJob;
                            gLastFailedJobMs = now;
                            resetCapturedState();
                        }
                    } else {
                        gConsecutiveRevalidateFailures = 0;
                    }
                }
                if (now - gLastSwitchCheckMs >= SWITCH_CHECK_MS) {
                    gLastSwitchCheckMs = now;
                    maybeSwitchToAlternateJob(false);
                }
                if (gSearchStage == STAGE_READY && gCapturedLuaState && gCapturedScriptContext &&
                    now - gReadySinceMs >= VM_TOUCH_GRACE_MS) {
                    if (!gElevatedThisCapture && elevateThreadCapabilities(gCapturedLuaState)) {
                        gElevatedThisCapture = true;
                    }
                    if (gElevatedThisCapture) {
                        uint64_t ownerBefore = 0;
                        safeReadBytes((const uint8_t *)gCapturedScriptContext + 0x658, &ownerBefore, sizeof(ownerBefore));

                        PluginLoader_RunPendingIfAny();
                        DiscordPresence_Tick();

                        uint64_t ownerAfter = 0;
                        safeReadBytes((const uint8_t *)gCapturedScriptContext + 0x658, &ownerAfter, sizeof(ownerAfter));
                        if (ownerBefore != ownerAfter) {
                            logmsg("diag: sc+0x658 owner-thread changed by our own VM call! before=0x%llx after=0x%llx calling pthread_self=%p\n",
                                   (unsigned long long)ownerBefore, (unsigned long long)ownerAfter, (void *)pthread_self());
                        }
                    }
                }
            }
        } else {
            trackAlternateJob(self);
            gAlternateJobCandidate = self;

            uint64_t now = monotonicMillis();
            if (gCapturedDataModelJob && gLastCapturedJobStepMs != 0 &&
                now - gLastCapturedJobStepMs > CAPTURED_JOB_STALE_MS &&
                now - gLastSwitchCheckMs >= SWITCH_CHECK_MS) {
                gLastSwitchCheckMs = now;
                logmsg("diag: captured job %p appears stale (no step in %llums) - forcing re-evaluation\n",
                       gCapturedDataModelJob, (unsigned long long)(now - gLastCapturedJobStepMs));
                maybeSwitchToAlternateJob(true);
            }
        }
    }
    return ((StepFnType)gOriginalStepFn)(self, stats);
}

static bool installStepHook(void) {
    findMainImage();
    if (!gImageHeader) {
        logmsg("main image not found\n");
        return false;
    }

    std::vector<SegRange> textSegs = getSegmentsByPrefix("__TEXT");
    std::vector<SegRange> dataSegs = getSegmentsByPrefix("__DATA");

    std::vector<uintptr_t> stepHits = findPatternMatches(textSegs, parsePattern(STEP_SIGNATURE));
    logmsg("step signature matches: %zu\n", stepHits.size());
    if (stepHits.empty()) {
        logmsg("DataModelJob::step not found - Roblox Studio likely updated and this signature needs refreshing\n");
        return false;
    }
    if (stepHits.size() > 1) {
        logmsg("WARNING: step signature ambiguous (%zu matches), using first\n", stepHits.size());
    }
    uintptr_t stepAddr = stepHits[0];
    gOriginalStepFn = (void *)stepAddr;
    logmsg("DataModelJob::step located @ 0x%lx\n", (unsigned long)stepAddr);

    gDataModelVtable = findPrimaryVtable(DATAMODEL_RTTI, textSegs, dataSegs);
    gScriptContextVtable = findPrimaryVtable(SCRIPTCONTEXT_RTTI, textSegs, dataSegs);

    std::vector<uintptr_t> loadWrapHits = findPatternMatches(textSegs, parsePattern(LUAU_LOAD_WRAPPER_SIGNATURE));
    logmsg("luau_load wrapper signature matches: %zu\n", loadWrapHits.size());
    if (loadWrapHits.size() == 1) {
        gLuauLoadWrapperFn = (void *)loadWrapHits[0];
        logmsg("luau_load wrapper located @ 0x%lx\n", (unsigned long)loadWrapHits[0]);
    } else if (loadWrapHits.size() > 1) {
        logmsg("WARNING: luau_load wrapper signature ambiguous (%zu matches), not using it\n", loadWrapHits.size());
    }

    std::vector<uintptr_t> callHits = findPatternMatches(textSegs, parsePattern(CALL_DISPATCH_SIGNATURE));
    logmsg("call dispatch signature matches: %zu\n", callHits.size());
    if (callHits.size() == 1) {
        gCallDispatchFn = (void *)callHits[0];
        logmsg("call dispatch located @ 0x%lx\n", (unsigned long)callHits[0]);
    } else if (callHits.size() > 1) {
        logmsg("WARNING: call dispatch signature ambiguous (%zu matches), not using it\n", callHits.size());
    }

    std::vector<uintptr_t> taskDeferHits = findPatternMatches(textSegs, parsePattern(TASK_DEFER_SIGNATURE));
    logmsg("task_defer signature matches: %zu\n", taskDeferHits.size());
    if (taskDeferHits.size() == 1) {
        gTaskDeferFn = (void *)taskDeferHits[0];
        logmsg("task_defer located @ 0x%lx\n", (unsigned long)taskDeferHits[0]);
    } else if (taskDeferHits.size() > 1) {
        logmsg("WARNING: task_defer signature ambiguous (%zu matches), not using it\n", taskDeferHits.size());
    }

    std::vector<uintptr_t> newthreadHits = findPatternMatches(textSegs, parsePattern(LUA_NEWTHREAD_SIGNATURE));
    logmsg("lua_newthread signature matches: %zu\n", newthreadHits.size());
    if (newthreadHits.size() == 1) {
        gLuaNewthreadFn = (void *)newthreadHits[0];
        logmsg("lua_newthread located @ 0x%lx\n", (unsigned long)newthreadHits[0]);
    } else if (newthreadHits.size() > 1) {
        logmsg("WARNING: lua_newthread signature ambiguous (%zu matches), not using it\n", newthreadHits.size());
    }

    int hookedCount = 0;
    for (const char *rttiName : JOB_CLASS_CANDIDATES) {
        uintptr_t vtableBase = findPrimaryVtable(rttiName, textSegs, dataSegs);
        if (!vtableBase) continue;

        if (strcmp(rttiName, "N3RBX19ScriptContextFacets23WaitingHybridScriptsJobE") == 0) {
            gWaitingHybridScriptsJobVtable = vtableBase;
        }

        int slotIndex = findSlotIndexForFunction(vtableBase, stepAddr, 12);
        if (slotIndex < 0) {
            logmsg("  %s: step not present in this vtable, skipping\n", rttiName);
            continue;
        }
        uintptr_t slotAddr = vtableBase + (uintptr_t)slotIndex * 8;
        if (patchVtableSlot(slotAddr, (void *)hookedStep)) {
            logmsg("  %s: hooked slot %d (0x%lx)\n", rttiName, slotIndex, (unsigned long)slotAddr);
            hookedCount++;
        }
    }

    logmsg("hook install complete, %d vtable(s) patched\n", hookedCount);
    return hookedCount > 0;
}

#pragma mark - Entry point

__attribute__((constructor))
static void bootstrap(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        gLog = fopen("/tmp/studio_patcher_datamodeljob_hook.txt", "w");
        LuauRuntime_Init();
        PluginLoader_Start();
        if (strlen(DISCORD_CLIENT_ID) > 0) {
            DiscordPresence_Start(DISCORD_CLIENT_ID);
        }
        installStepHook();
    });
}
