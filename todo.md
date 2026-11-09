# Keyboard Customizer (Tauri 2, macOS)

## Goal
A personal macOS menu bar app that manages three keyboard tweaks with on/off switches: (1) the Colemak DH ANSI layout, (2) swapping Caps Lock and Backspace, (3) making the keyboard's lock key put the Mac to sleep instead of locking.

## Context
- Machine: Mac mini (Apple Silicon, `Mac16,10`), macOS 27. Keyboard: Logitech ERGO K860 over Bluetooth LE (VendorID `0x46d`, ProductID `0xb359`). Logi Options+ is NOT installed.
- Toolchain on the machine: `cargo`/`rustc` (rustup), `bun`, `node`. Use **bun** as the JS package manager.
- The project dir `/Users/bryan/src/keyboard-customizer-tauri` is empty and is not a git repo.
- Current system state (the app must manage this):
  - `~/Library/Keyboard Layouts/Colemak DH.bundle` is installed. It contains several layouts; the one in use is **"Colemak DH ANSI"** (`KeyboardLayout ID = -7379`), and it is the active input source.
  - Caps↔Backspace is already applied through `hidutil` (`UserKeyMapping`: `0x700000039` ↔ `0x70000002A`, decimal `30064771129` ↔ `30064771114`). No LaunchAgent sets it, so it is lost on reboot.
- Commands:
  - Dev: `bun run tauri dev`
  - Build: `bun run tauri build` (creates the `.app` in `src-tauri/target/release/bundle/macos/`)
  - Rust tests: `cd src-tauri && cargo test`
  - Lint: `cd src-tauri && cargo clippy -- -D warnings`, `cargo fmt --check`
- Useful verification commands:
  - `hidutil property --get UserKeyMapping`
  - `defaults read com.apple.HIToolbox AppleSelectedInputSources`
  - `launchctl list | grep keyboard-customizer`

## Decisions
- **Stack**: Tauri 2 + Rust backend. Frontend is plain TypeScript + Vite (template `vanilla-ts`), no UI framework. Package manager is bun.
- **App identifier**: `com.bryan.keyboard-customizer`. Product name: `Keyboard Customizer`.
- **App shape**: a menu bar (tray) app with no Dock icon (`ActivationPolicy::Accessory`). The tray menu has "Settings…" and "Quit". "Settings…" opens one small window (about 420×320, not resizable) with three switches and a status line for each. Closing the window hides it; the app keeps running.
- **Launch at login**: on by default, using `tauri-plugin-autostart` (`MacosLauncher::LaunchAgent`).
- **Scope of remaps**: all keyboards, including ones connected later. No per-device filtering.
- **Feature 1, Colemak DH**: use the macOS input source (keylayout). Do NOT remap letter keys in the app.
  - ON: if the bundle is missing from `~/Library/Keyboard Layouts/`, copy it there from the app resources. Then register, enable, and select the input source whose localized name is exactly `Colemak DH ANSI` using the Text Input Sources (TIS) API in Carbon/HIToolbox.
  - OFF: select the input source with ID `com.apple.keylayout.ABC`. Do not delete the bundle.
  - Status: show whether `Colemak DH ANSI` is the currently selected input source.
- **Feature 2, Caps↔Backspace**: use `hidutil` `UserKeyMapping`, with these exact values: Src `0x700000039` → Dst `0x70000002A`, and Src `0x70000002A` → Dst `0x700000039`.
  - ON: (a) run `/usr/bin/hidutil property --set '<json>'` right away, and (b) write the LaunchAgent `~/Library/LaunchAgents/com.bryan.keyboard-customizer.capsbackspace.plist`, which runs the same command at login (`RunAtLoad = true`, no `KeepAlive`). Then `launchctl bootstrap gui/$(id -u) <plist>` (ignore the "already loaded" error). While the app is running (or not yet started at login), the swap does not depend on it; quitting the app removes it (see below).
  - OFF: `launchctl bootout gui/$(id -u)/com.bryan.keyboard-customizer.capsbackspace` (ignore "not loaded"), delete the plist, then run `/usr/bin/hidutil property --set '{"UserKeyMapping":[]}'`.
  - Status: ON when `hidutil property --get UserKeyMapping` contains both mappings AND the plist exists.
- **Feature 3, Lock key → Sleep**: use a `CGEventTap` (session tap, head insert, active/default, not listen-only). It requires the Accessibility permission and only works while the app is running.
  - The exact event the K860 lock key sends is unknown. The app has a **Learn** button: the next key event it sees (keyDown, flagsChanged, or `NX_SYSDEFINED`/type 14) is saved as the trigger.
  - When the saved trigger fires: swallow the event (return null) and its matching key-up, then run `/usr/bin/pmset sleepnow` (immediate system sleep, no lock step first).
  - Trigger on key-down only. Ignore repeats (`kCGKeyboardEventAutorepeat`).
  - If the tap is disabled (`kCGEventTapDisabledByTimeout` / `ByUserInput`), turn it back on with `CGEventTapEnable(tap, true)`.
- **Persistence**: config is JSON at `<app_config_dir>/config.json` (Tauri `path().app_config_dir()`). Schema:
  ```json
  {
    "colemak": true,
    "caps_backspace": true,
    "lock_sleep": false,
    "lock_trigger": null
  }
  ```
  `lock_trigger` is either `null` or one of:
  - `{"kind":"key","keycode":12,"flags":1310720}`, where `flags` is masked to `Shift|Control|Alternate|Command|SecondaryFn` (`0x00FF0000` range bits only).
  - `{"kind":"system","subtype":8,"data1_keycode":N}`, where `data1_keycode = (data1 & 0xFFFF0000) >> 16` from the NSEvent `data1` of the `NX_SYSDEFINED` event.
- **Defaults on first run** (no config file): `colemak: true`, `caps_backspace: true`, `lock_sleep: false` (it can't work until a key is learned).
- **On app start**: apply each saved ON setting so the system matches the config. Do NOT apply OFF settings on start. The app reverts a feature when the user switches it off, and on quit.
- **On quit** (tray Quit, Cmd+Q, logout; handled in `RunEvent::Exit`): revert Colemak (select ABC) and Caps↔Backspace (same steps as OFF, including removing the LaunchAgent) if they are ON, then exit. Do NOT change the config, so the next launch re-applies them. Crash or force-quit do not revert.
- **Distribution**: local, unsigned build only. Use ad-hoc signing (`bundle.macOS.signingIdentity = "-"`). No notarization, no updater, no DMG work.
- **Colemak bundle source**: copy `~/Library/Keyboard Layouts/Colemak DH.bundle` from this machine into `src-tauri/resources/Colemak DH.bundle` and list it under `bundle.resources` in `tauri.conf.json`.

## Tasks
Tick each box as you finish it. Do NOT commit to git. The user approves commits.

### M1: Scaffold
- [x] 1. In `/Users/bryan/src/keyboard-customizer-tauri`, scaffold Tauri 2 with the `vanilla-ts` template and bun (`bun create tauri-app`). Name `keyboard-customizer`, identifier `com.bryan.keyboard-customizer`. Keep `todo.md`. Run `git init` (no commit).
- [x] 2. In `src-tauri/tauri.conf.json`: product name `Keyboard Customizer`. The main window is `"visible": false`, 420×320, `"resizable": false`, title "Keyboard Customizer". Set `bundle.macOS.signingIdentity: "-"` and `bundle.resources` to include `resources/Colemak DH.bundle`.
- [x] 3. Copy the Colemak bundle into `src-tauri/resources/` (see Decisions).
- [x] 4. Add the Rust deps: `tauri` (feature `tray-icon`), `tauri-plugin-autostart`, `serde`, `serde_json`, `core-graphics`, `core-foundation`, `plist`. No other crates.
- [x] 5. In `src-tauri/src/lib.rs`: set `ActivationPolicy::Accessory`. Build the tray icon with the menu "Settings…" (show and focus the window) and "Quit" (`app.exit(0)`). Intercept the window `CloseRequested` event so it hides the window instead. Register the autostart plugin and enable it on first run.
- [ ] 6. Verify: `bun run tauri dev` shows a tray icon and no Dock icon, the window opens and hides, and Quit reverts Colemak and Caps↔Backspace, then exits.

### M2: Config + Caps↔Backspace
- [x] 7. `src-tauri/src/config.rs`: a `Config` struct matching the schema, `LockTrigger` enum (`#[serde(tag = "kind", rename_all = "lowercase")]`), plus `load(path) -> Config` (missing file → defaults) and `save(path, &Config)`. Write atomically (temp file + rename).
- [x] 8. `src-tauri/src/caps.rs`:
  - `mapping_json() -> String`: the exact `{"UserKeyMapping":[...]}` JSON with the decimal values `30064771129`/`30064771114`.
  - `launch_agent_plist(hidutil_json: &str) -> Vec<u8>`: label `com.bryan.keyboard-customizer.capsbackspace`, `ProgramArguments = ["/usr/bin/hidutil","property","--set",<json>]`, `RunAtLoad = true`.
  - `enable() -> Result<()>`, `disable() -> Result<()>`, `status() -> bool`, as described in Decisions.
- [x] 9. Unit tests for `mapping_json`, `launch_agent_plist` (parse it back with the `plist` crate and check the fields), and config load/save round-trip including both `LockTrigger` variants.

### M3: Colemak DH
- [x] 10. `src-tauri/src/input_source.rs`: raw FFI to `Carbon.framework` (`#[link(name = "Carbon", kind = "framework")]`) for `TISCreateInputSourceList`, `TISGetInputSourceProperty`, `TISRegisterInputSource`, `TISEnableInputSource`, `TISSelectInputSource`, `TISCopyCurrentKeyboardInputSource`, and the property keys `kTISPropertyLocalizedName`, `kTISPropertyInputSourceID`. Use `includeAllInstalled = true` when searching.
- [x] 11. Functions:
  - `ensure_bundle_installed(resource_dir) -> Result<bool>`: copies the bundle if missing and returns `true` if it copied. After a copy, call `TISRegisterInputSource` with the bundle URL.
  - `select_by_name("Colemak DH ANSI") -> Result<()>`: enable, then select.
  - `select_by_id("com.apple.keylayout.ABC") -> Result<()>`.
  - `current_name() -> Option<String>`.
- [x] 12. Run every TIS call on the main thread (`app.run_on_main_thread`). If the source isn't found after installing, return the error "Colemak DH ANSI not found. Log out and back in, then try again." and show it in the UI.

### M4: Lock key → Sleep
- [x] 13. `src-tauri/src/lock_tap.rs`: create the tap on a dedicated thread with its own `CFRunLoop`. Event mask: `KeyDown | KeyUp | FlagsChanged | 14 (NX_SYSDEFINED)`. Share state (`Arc<Mutex<..>>`): `enabled: bool`, `trigger: Option<LockTrigger>`, `learning: bool`.
- [x] 14. Learn mode: when `learning` is true, the next event (keyDown, or `NX_SYSDEFINED` with subtype 8 and key-down state) becomes the trigger. Swallow that event, save it to the config, set `learning = false`, and emit the Tauri event `lock-learned` with a readable description (for example `keycode 12 + ⌃⌘` or `system key 6`).
- [x] 15. Match mode: if `enabled` and the event matches the trigger (key: same keycode and the same masked flags; system: same subtype and `data1_keycode`), swallow it. On key-down, spawn `/usr/bin/pmset sleepnow`. Also swallow the matching key-up.
- [x] 16. Accessibility: check with `AXIsProcessTrustedWithOptions` (prompt = true) before creating the tap. If permission is missing, the status shows "Needs Accessibility permission", and a button opens `x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility`.
- [x] 17. Unit tests for trigger matching (pure function `matches(trigger, event_summary) -> bool` using a plain struct, not real CGEvents) and for flag masking.

### M5: UI + wiring
- [x] 18. Tauri commands in `lib.rs`: `get_status() -> Status`, `set_colemak(on: bool)`, `set_caps_backspace(on: bool)`, `set_lock_sleep(on: bool)`, `start_learn()`, `open_accessibility_settings()`. Each `set_*` applies the change, saves the config, and returns the new `Status`. Errors come back as strings.
- [x] 19. `Status` per feature: `{ on: bool, ok: bool, message: String }`. Lock also includes `trigger_label: Option<String>` and `accessibility: bool`.
- [x] 20. `src/main.ts` + `index.html`: three rows (label, switch, status message). The lock row also has a "Learn key" button (shows "Press the lock key…" while learning and listens for `lock-learned`) and, when needed, an "Open Accessibility settings" button. The lock switch is disabled until a trigger has been learned. Plain CSS, system font, light/dark via `prefers-color-scheme`.
- [x] 21. On startup, apply the ON settings from the config (see Decisions) and start the tap if `lock_sleep` is on.

## Feature checklist
- [ ] Colemak DH ANSI selectable on/off from the app
- [ ] Caps↔Backspace swap on/off, survives reboot without the app running
- [ ] Lock key puts the Mac to sleep (learned key, app running)
- [ ] Menu bar app, no Dock icon, launches at login

## Acceptance criteria
- `cd src-tauri && cargo test` passes. `cargo clippy -- -D warnings` and `cargo fmt --check` are clean. `bun run tauri build` produces `Keyboard Customizer.app`.
- Caps ON: `hidutil property --get UserKeyMapping` shows both mappings, `~/Library/LaunchAgents/com.bryan.keyboard-customizer.capsbackspace.plist` exists, and pressing Caps deletes a character while Backspace toggles caps. After a reboot with the app still running, the swap still works. After quitting, the swap is gone and the plist is removed.
- Caps OFF: the plist is gone, `UserKeyMapping` is empty, and the keys behave normally.
- Colemak ON: `defaults read com.apple.HIToolbox AppleSelectedInputSources` shows `Colemak DH ANSI`, and typing physical QWERTY `sdf` gives `rst`. Colemak OFF: ABC is selected.
- Lock: after Learn plus one press of the K860 lock key, the UI shows a trigger label. With the switch ON, pressing the lock key sleeps the Mac and does not show the lock screen first. With the switch OFF, the key behaves as before.
- The app shows a tray icon only (no Dock icon) and is in Login Items after first run.

## Out of scope
- Any other remaps, layers, per-app or per-device rules, or a key-remap editor.
- Remapping letters in the app (the keylayout handles Colemak).
- Signing with a real identity, notarization, auto-update, DMG/installer.
- Windows/Linux support.
- Any new deps beyond those listed in task 4.

## Notes / risks
- **Accessibility vs. rebuilds**: ad-hoc-signed builds get a new code hash on every build, and macOS may silently drop the Accessibility grant. After rebuilding, remove and re-add the app in System Settings → Privacy & Security → Accessibility. Test the lock feature with the built `.app`, not `tauri dev` (in dev mode the terminal app is the one that needs the permission).
- **Lock key may not reach the event tap**: if Learn sees no event when the K860 lock key is pressed, the key is handled below the event-tap level. Do not try workarounds (IOHID seizing, drivers, Karabiner). Show the UI message "Lock key not detectable by this app" and note the limitation in a `README.md`.
- **If the lock key sends ⌃⌘Q**: swallowing it also stops ⌃⌘Q typed by hand from locking. That is expected.
- **Virtual keycodes are physical positions**, so a trigger learned under Colemak still matches under ABC.
- **`hidutil` OFF clears all `UserKeyMapping`s.** That is OK because this is the only mapping on this machine.
- **TIS calls are not thread-safe**: always run them on the main thread.
- A newly copied keylayout may not show up until logout/login (see task 12). Colemak is already installed on this machine, so this mostly doesn't matter here.
- Don't `launchctl bootstrap` from inside tests. Unit tests only cover pure functions.
