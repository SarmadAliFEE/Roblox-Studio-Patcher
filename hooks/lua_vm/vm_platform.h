#pragma once

#include <cstddef>
#include <cstdint>

bool VM_IsReady(void);

bool VM_IsPlayTestActive(void);

void *VM_GetLuaState(void);

void VM_InvalidateCapturedState(void);

typedef int (*VM_LuauLoadFn)(void *L, const char *chunkname, const uint8_t *data, size_t size, int env);
typedef uint64_t (*VM_CallDispatchFn)(void *L, uint64_t param2, int nargs);
VM_LuauLoadFn VM_GetLuauLoadFn(void);
VM_CallDispatchFn VM_GetCallDispatchFn(void);

typedef uint64_t (*VM_TaskDeferFn)(void *L);
VM_TaskDeferFn VM_GetTaskDeferFn(void);

typedef void *(*VM_LuaNewthreadFn)(void *L);
VM_LuaNewthreadFn VM_GetLuaNewthreadFn(void);

bool VM_SafeReadBytes(const void *addr, void *out, size_t n);

bool VM_SafeWriteBytes(void *addr, const void *in, size_t n);

void VM_Log(const char *fmt, ...);

void VM_DumpProtoBytes(void *closurePtr);


void VM_TestProtoSentinelWrite(void *closurePtr);

void VM_ElevateClosureCapabilities(void *closurePtr);

void VM_ElevateThreadCapabilities(void *L);

void VM_ElevateSecurityContext(void);

void *VM_FindStudioServiceInstance(void);

void VM_PushInstance(void *L, void *instancePtr);
