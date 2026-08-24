#pragma once

#include <cstdint>

void DiscordIpc_Start(const char *clientId);

void DiscordIpc_SetActivity(const char *state, const char *details, int64_t startTimestampUnix,
                             const char *largeImageKey, const char *largeImageText,
                             const char *smallImageKey, const char *smallImageText,
                             const char *buttonLabel = "", const char *buttonUrl = "");
