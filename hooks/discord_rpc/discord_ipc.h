// Raw Discord local-IPC client (the same Unix-domain-socket protocol the
// official Game SDK wraps) - hand-rolled instead of vendoring Discord's
// closed-source SDK binary, consistent with how every other hook in this
// project touches its target directly. POSIX/macOS-Linux only for now
// (Windows would need a named-pipe transport instead); nothing above this
// interface needs to know that.
#pragma once

#include <cstdint>

// Starts a background thread that connects to Discord's local IPC socket
// and performs the RICH_PRESENCE handshake. Connection retries on its own
// on a fixed interval until Discord is actually running, so this is safe
// to call once at startup even if Discord isn't open yet. Idempotent.
void DiscordIpc_Start(const char *clientId);

// buttonLabel/buttonUrl add a single clickable button below the activity -
// Discord's Rich Presence has no way to make the details/state text or the
// images themselves clickable, a button is the actual supported mechanism
// for "click to go here". Pass empty strings for neither (no button).
void DiscordIpc_SetActivity(const char *state, const char *details, int64_t startTimestampUnix,
                             const char *largeImageKey, const char *largeImageText,
                             const char *smallImageKey, const char *smallImageText,
                             const char *buttonLabel = "", const char *buttonUrl = "");
