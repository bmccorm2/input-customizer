# Keyboard Customizer

macOS menu bar app (Tauri 2) with three switches:

1. **Colemak DH ANSI**: selects the keylayout from `~/Library/Keyboard Layouts/Colemak DH.bundle` (installed from app resources if missing). Off selects ABC.
2. **Caps Lock ↔ Backspace**: `hidutil` UserKeyMapping, plus the LaunchAgent `com.bryan.keyboard-customizer.capsbackspace` so it survives reboot without the app.
3. **Lock key → Sleep**: a CGEventTap swallows the learned key and runs `pmset sleepnow`. It needs Accessibility permission and works only while the app is running.

Config: `~/Library/Application Support/com.bryan.keyboard-customizer/config.json`.

## Commands

```fish
bun run tauri dev      # dev
bun run tauri build    # src-tauri/target/release/bundle/macos/Keyboard Customizer.app
bun run deploy         # build, install to /Applications, relaunch
cd src-tauri; and cargo test; and cargo clippy -- -D warnings; and cargo fmt --check
```

## Notes

- **Accessibility after rebuilds**: builds are signed with the self-signed cert "Keyboard Customizer Dev" (login keychain), so the code requirement is stable and the grant survives rebuilds. Losing or recreating that cert means re-granting once. `tauri dev` binaries aren't signed with it, so test the event tap with `bun run deploy`.
- **Lock key may be invisible to the app**: if Learn sees nothing within 15 s, the UI shows "Lock key not detectable by this app". That means the K860 lock key is handled below the event-tap level, and this app can't remap it.
- If the learned trigger is ⌃⌘Q, typing ⌃⌘Q by hand also sleeps instead of locking.
- Turning Caps↔Backspace off clears **all** `hidutil` UserKeyMappings.
- Launch at login is registered on the first release-build launch. While it stays enabled, each launch re-points it at the running copy (`/Applications` after `bun run deploy`).
