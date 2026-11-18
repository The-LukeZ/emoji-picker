# Manual testing

Checks that need a person at the keyboard. Tick each box after verifying it on a real desktop.

## Service thread, tray and hotkey gating (Task 11)

Setup: a debug build (logs go to stderr) with this config. Debug builds read the config from
`EMOJI_PICKER_CONFIG_DIR` when it is set, otherwise from `%APPDATA%\emoji-picker\config.toml`.

```toml
hotkey = "Ctrl+Shift+Space"
blocklist = ["notepad.exe"]
```

- [ ] Tray icon shows; menu Quit exits cleanly.
- [ ] Hotkey in Explorer logs `OpenPicker`; in Notepad nothing is logged, and Notepad receives Ctrl+Shift+Space.
- [ ] A fullscreen browser video blocks the hotkey; leaving fullscreen restores it.
- [x] Same window going fullscreen without a focus change: browser video fullscreen and F11 in a browser that is already in front block the hotkey (the combo reaches the browser); leaving fullscreen restores it without switching windows. (Verified automatically with a borderless WinForms window resized in place, not with a real browser.)
- [ ] Starting a second `emoji-picker.exe` exits it at once, logs `OpenSettings` in the first, and only one tray icon exists.
- [ ] Another app holding Ctrl+Shift+Space (register it in a scratch program) gives the balloon.

## Picker window (Task 12)

Setup: a debug build with `hotkey = "Ctrl+Shift+Space"` (or a release build with
`--features diag`, run as `emoji-picker.exe 2> log.txt`, which also logs). Open and close times
and private bytes are in the log lines `picker: open N ms (private K KB, ...)` and
`picker: hidden (private K KB)`. `EMOJI_PICKER_SELFTEST=10` opens and closes the picker 10 times
by itself (no synthetic keys), logs the same numbers and a summary, then quits.

- [ ] Hotkey opens the picker next to the Notepad caret.
- [ ] With the caret at the bottom of the screen the picker opens above it.
- [ ] Typing `fire` filters on each key.
- [ ] Arrows move the selection, Tab and Shift+Tab switch categories, Esc closes.
- [ ] Ctrl+wheel changes the size through all 4 steps.
- [ ] Dark mode follows Windows (Settings > Personalization > Colors).
- [ ] Clicking the hand swatch cycles the tones and thumbs up changes with it.
- [ ] No taskbar button and no Alt+Tab entry while the picker is open.
- [ ] Clicking outside the picker closes it.
- [ ] Open and close 10 times, then read private bytes from the debug log: hidden < 30 MB,
      open < 60 MB; median open < 200 ms. Numbers recorded in
      `.superpowers/sdd/2026-11-18-emoji-picker/spike-results.md`.
