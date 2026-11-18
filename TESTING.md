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
- [ ] Same window going fullscreen without a focus change: browser video fullscreen and F11 in a browser that is already in front block the hotkey (the combo reaches the browser); leaving fullscreen restores it without switching windows.
- [ ] Starting a second `emoji-picker.exe` exits it at once, logs `OpenSettings` in the first, and only one tray icon exists.
- [ ] Another app holding Ctrl+Shift+Space (register it in a scratch program) gives the balloon.
