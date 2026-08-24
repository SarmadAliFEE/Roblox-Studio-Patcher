#pragma once

#include <string>

void LuauRuntime_Init(void);

bool LuauRuntime_Run(const std::string &source, const char *chunkname, std::string &outResult);

bool LuauRuntime_RunSync(const std::string &source, const char *chunkname, std::string &outResult);

void *LuauRuntime_LoadPersistent(const std::string &source, const char *chunkname);

bool LuauRuntime_CallPersistent(void *handle, std::string &outResult);

bool LuauRuntime_ExposeStudioService(std::string &outResult);
