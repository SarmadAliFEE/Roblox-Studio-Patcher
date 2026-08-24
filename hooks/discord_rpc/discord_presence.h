// Gathers place name / active script / cursor position from Studio's own
// live Luau VM (via luau_runtime.h) on a cheap cadence and forwards changes
// to Discord Rich Presence through discord_ipc.h. Pure portable C++ - talks
// to the VM only through vm_platform.h/luau_runtime.h, so this file has no
// OS-specific code of its own and is shared as-is between every platform.
#pragma once

// Starts the underlying IPC connection (see discord_ipc.h) and records the
// session start time used for the Rich Presence "elapsed" timer. Call once
// from the platform layer's own startup.
void DiscordPresence_Start(const char *clientId);

// Call from the VM's own calling thread (same convention as
// PluginLoader_RunPendingIfAny) on every step - internally rate-limited to
// POLL_INTERVAL_MS, so cheap on ticks where it does nothing.
void DiscordPresence_Tick(void);
