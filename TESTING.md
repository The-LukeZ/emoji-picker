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
- [x] Typing `fire` filters on each key. (Verified 2026-11-18: real hotkey from Notepad, keys
      sent with `SendInput`; the log shows a search for `f`, `fi`, `fir`, `fire`, 11 hits.)
- [x] The first hotkey open takes the foreground (typing goes to the search field), Esc closes
      and gives the foreground back to Notepad, and a second hotkey press while open closes the
      picker. (Verified 2026-11-18 with `SendInput`, foreground checked with
      `GetForegroundWindow`.)
- [ ] Arrows move the selection, Tab and Shift+Tab switch categories, Esc closes.
- [ ] Ctrl+wheel changes the size through all 4 steps.
- [ ] Dark mode follows Windows (Settings > Personalization > Colors).
- [ ] Clicking the hand swatch cycles the tones and thumbs up changes with it.
- [ ] No taskbar button and no Alt+Tab entry while the picker is open.
- [ ] Clicking outside the picker closes it.
- [x] Open and close 10 times, then read private bytes from the debug log: hidden < 30 MB,
      open < 60 MB; median open < 200 ms. Numbers recorded in
      `.superpowers/sdd/2026-11-18-emoji-picker/spike-results.md`. (Verified 2026-11-18 with
      `EMOJI_PICKER_SELFTEST=10` on a release `diag` build: opens go through `Picker::open`,
      not the hotkey, so OS hotkey dispatch is not included.)

Note: pressing the hotkey while the picker is open closes it (toggle, owner decision
2026-11-18).

## Win+. hook (Task 13)

Setup: a debug build (logs go to stderr) with `hotkey = "Win+."` in the config. The log shows
`ll-hook: installed` / `ll-hook: uninstalled` whenever the hook comes and goes, and
`service: session change N` on lock and unlock. Desktop tests must never press a bare
modifier (Alt, Win, Ctrl, Shift) to win the foreground unless it is the only reliable way;
then the script documents why, releases it, and checks `GetAsyncKeyState` afterwards. Every
modifier pressed must be released, and the test ends by checking that Win, Ctrl, Shift and
Alt are all up.

- [ ] Win+. opens our picker and the Windows emoji panel does not open.
- [ ] Releasing Win after Win+. does not open the Start menu.
- [ ] Win alone still opens the Start menu.
- [ ] Win+E still opens Explorer.
- [ ] In a blocklisted game Win+. reaches the game (the Windows panel opens there), and the
      log (or Process Explorer) shows the hook uninstalled while the game has focus.
- [ ] The picker gets keyboard focus every time across 20 Win+. opens (typing goes to the
      search field; the log has no `SetForegroundWindow refused`).
- [ ] Win+L, unlock, then type `.` in Notepad: a period is typed and no picker opens.
- [ ] Win+L, unlock, then Win+. still opens the picker (the matcher was reset, not stuck).
- [ ] Making `SetWindowsHookExW` fail (or a debugger forcing `is_lost`) shows the balloon
      "Win+. could not be captured, switching to Ctrl+Shift+Space", and `config.toml` then has
      `hotkey = "Ctrl+Shift+Space"` with tone and size unchanged.
