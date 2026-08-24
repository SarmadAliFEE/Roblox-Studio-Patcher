# Roblox-Studio-Patcher

![platform](https://img.shields.io/badge/platform-macOS%20%7C%20windows-blue) ![built in rust](https://img.shields.io/badge/built%20in-rust-orange) ![deps none](https://img.shields.io/badge/runtime%20deps-none-brightgreen)

flips Studio's `HasInternalPermission` to always-true, plus a handful of optional native hooks. one binary, no install, nothing to compile. mac (arm64) + windows.

## what it does

Studio has a hidden internal mode normally reserved for Roblox employees and their "Soothsayer" testers - debug tools, experimental features, internal-only APIs and menus you don't normally get. `HasInternalPermission` is the check that gates it; this patches it open.

(related but separate: the command bar has an `ElevatedStudioPlugin` script identity next to the usual `CommandBar` one - same idea of elevated access, different mechanism, not what this touches.)

on top of the permission patch, opt into any of:

- **custom themes** - edit Studio's qt theme jsons live off disk
- **plugin palette** - recolor the Explorer / ribbon / find-replace plugins, colors baked into their bytecode
- **script editor background** - a custom image behind your code
- **window transparency** - hotkeys to fade the whole window
- **discord rich presence** - the place, script, and line you're editing *(mac for now)*
- **webhook logging** - mirror every error and crash to a discord webhook

## usage

grab your build from [releases](https://github.com/uwufuzzywiiiaisddd/Roblox-Studio-Patcher/releases).

**mac (arm64)**

```bash
chmod +x Roblox-Studio-Patcher-mac-silicon
./Roblox-Studio-Patcher-mac-silicon                                    # patches /Applications/RobloxStudio.app
./Roblox-Studio-Patcher-mac-silicon --binary /path/to/RobloxStudio.app # or a custom path
```

**windows**

```cmd
Roblox-Studio-Patcher-windows.exe
```

finds Studio under `%LOCALAPPDATA%\Roblox\Versions` on its own, or pass `--binary path\to\RobloxStudioBeta.exe`.

just run it - a plain run walks through each option below and asks yes/no. backs up the original binary first (`.bak-<timestamp>` next to it), and checks for a newer release before starting (`--update` to only do that, say no to keep what you've got).

## the extras

all opt-in - the default run asks, or reach for the flag.

**themes** (`--themes`) redirects Studio's theme jsons onto disk (`/Users/Shared/rbx-theme-set/` on mac, `C:\Users\Public\rbxthemeset` on windows) so you can edit and relaunch. drops the stock jsons there on first run. edit `FoundationDarkTheme.json` / `FoundationLightTheme.json`, whichever one Studio's actually using, then relaunch.

**plugin colors** (`--rbxm-palette`) the Explorer, ribbon, and find/replace plugins are Roact with colors baked into their compiled bytecode - the theme jsons don't reach them. this patches them from a `RbxmPalette` block in the same json and splices straight into the `.rbxm`. baked at patch time, so rerun after edits, or leave `--watch` running and it reapplies on save. still needs a full quit and reopen after.

**native hooks** one small rust payload injected through the binary's import table (doesn't touch running memory). the studio-hook cdylib is embedded in the exe, so there's nothing to build and no dll to ship. configs live in the theme-set folder above:

- script editor background - `EditorBackground.json`, blank `image` = off
- window transparency - `WindowTransparency.json`, ctrl+=/ctrl+- on mac, alt+=/alt+- on windows
- discord presence (`--discord`, mac for now) - place name, active script, cursor line, testing status, a thumbnail
- webhook logging (`--webhook-logging`) - forwards every log line, panic, and native crash to a discord webhook. crash reports are sent from a separate helper process, so they still go out even if the heap is corrupted. `Logger.json`

`--inject path/to/thing.dylib` / `.dll` loads your own hook instead.

## building

```bash
cargo build --release
```

windows from mac/linux needs the target + mingw (`rustup target add x86_64-pc-windows-gnu`, `brew install mingw-w64`), then `cargo build --release --target x86_64-pc-windows-gnu`. `.cargo/config.toml` routes that through `scripts/mingw-link-wrapper.sh` to statically link the bundled Luau compiler - without it the exe would want `libstdc++-6.dll` next to it to run.

## issues

DM [uwufuzzywiiiaisdd](https://discord.com/users/1382448091445203037) on discord.
