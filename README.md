# Roblox-Studio-Patcher

![platform](https://img.shields.io/badge/platform-macOS%20%7C%20windows-blue) ![rust](https://img.shields.io/badge/rust-orange)

Patches Roblox Studio so `HasInternalPermission` always returns true. mac + windows.

## What this actually does

Studio has a hidden internal mode normally reserved for Roblox employees and their "Soothsayer" testers - it unlocks debug tools, experimental features, and internal-only APIs/menus regular developers don't get. `HasInternalPermission` is the check that gates it; this patches it open for everyone.

Related: the command bar's script identity system has an `ElevatedStudioPlugin` identity level alongside the normal ones (`CommandBar`, etc.) - same general concept of elevated/internal access, just a separate mechanism from the permission check this tool patches.

## Usage

Grab the build for your OS from [releases](https://github.com/uwufuzzywiiiaisddd/Roblox-Studio-Patcher/releases).

**mac (arm64):**

```bash
chmod +x Roblox-Studio-Patcher-mac-silicon
./Roblox-Studio-Patcher-mac-silicon                          # patches /Applications/RobloxStudio.app
./Roblox-Studio-Patcher-mac-silicon --binary /path/to/RobloxStudio.app # or a custom path
```

**windows:**

```cmd
Roblox-Studio-Patcher-windows.exe
```

just run it, no install needed. finds your Studio install under `%LOCALAPPDATA%\Roblox\Versions` on its own, or pass `--binary path\to\RobloxStudioBeta.exe`.

A backup of the original binary is made before every patch (next to the original, `.bak-<timestamp>` on mac / same idea on windows).

Checks for a newer release on every plain run and asks before installing (`--update` to just do that and nothing else). Says no, keeps going with what you've got.

## Custom themes

The default run asks if you want this too, or just run it standalone with `--themes`.

redirects studio's theme jsons to a folder on disk instead of loading them baked into the binary (`/Users/Shared/rbx-theme-set/` on mac, `C:\Users\Public\rbxthemeset` on windows), so you can just edit em and relaunch. grabs the stock jsons for you on first run so you've got something to start from.

edit `FoundationDarkTheme.json` and `FoundationLightTheme.json` in that folder, whichever one studio's actually using, then just relaunch studio to see it

## Special plugin colors

Explorer, ribbon, and find/replace all are Roact plugins, colors baked into their compiled bytecode - the theme jsons above don't touch them.

`--rbxm-palette` patches those from a `RbxmPalette` block in the same json (auto-added with defaults). recompiles and splices straight into the `.rbxm`.

these are baked in at patch time, not read live, so editing the json means running `--rbxm-palette` again. `--watch` does that for you - leave it running, save the json, it reapplies. either way still needs a full quit and reopen of studio after, same as the qt colors.

## Native hooks

one small rust payload loaded into Studio at launch - patches the binary's import table, doesn't touch running memory. the default run asks about each, or `--inject path/to/thing.dylib` / `.dll` for your own.

all mac + windows unless noted. config jsons live in the theme-set folder above.

- **script editor background** - a custom image behind the code. `EditorBackground.json`, blank `image` = off.
- **window transparency** - hotkeys to fade studio's whole window. `WindowTransparency.json`, ctrl+=/ctrl+- on mac, alt+=/alt+- on windows.
- **discord rich presence** *(mac for now)* - shows the place, script, and cursor line you're on, testing status, a thumbnail. `--discord`.
- **webhook logging** - `--webhook-logging` mirrors every error and crash to a discord webhook. `Logger.json`.

## Building from source

```bash
cargo build --release
./target/release/studio-patcher
```

for a windows build from mac/linux you need the target + mingw (`rustup target add x86_64-pc-windows-gnu`, `brew install mingw-w64`), then `cargo build --release --target x86_64-pc-windows-gnu`. the repo's `.cargo/config.toml` routes that build through `scripts/mingw-link-wrapper.sh` so the bundled Luau compiler links statically - without it the exe would need `libstdc++-6.dll` next to it to run.

## Issues

DM [uwufuzzywiiiaisdd](https://discord.com/users/1382448091445203037) on Discord for any issues.
