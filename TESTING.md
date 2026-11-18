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
- [ ] A maximized normal window is not fullscreen: with the taskbar set to auto-hide, maximize
      Explorer (not blocklisted) and press the hotkey: the picker opens and the log shows
      `fullscreen=false`. Repeat on a second monitor that has no taskbar.
- [ ] Borderless fullscreen still blocks: F11 in a browser and a borderless game on the same
      monitors still log `fullscreen=true` and the hotkey reaches the app.
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

Automated runs: the hook ignores injected keys, so `SendInput` cannot drive it. Debug and
`diag` builds started with `EMOJI_PICKER_HOOK_TEST=1` treat injected keys as physical, except
keys whose `dwExtraInfo` is the app's own marker `0x454D4F4A` (its VK_E8 mask taps). Test
scripts must send their keys with `dwExtraInfo = 0`. The log shows
`ll-hook: test mode, injected keys count as physical` when the mode is on.

- [ ] Win+. opens our picker and the Windows emoji panel does not open.
- [ ] Releasing Win after Win+. does not open the Start menu.
- [x] Win alone still opens the Start menu, also as the first Win press after closing the
      picker with Esc. (Verified 2026-11-18, run by the owner in test mode with `SendInput`:
      Start (SearchHost.exe on this build) took the foreground on the first bare Win press; the
      log shows the Win key-up as `Pass` with no VK_E8 mask; Start was closed again with Esc.)
- [x] While the picker is open the hook still sees keys: Win+. closes our picker and the
      Windows emoji panel does not open. (Verified 2026-11-18, run by the owner in test mode
      with `SendInput`: the second Win+. was traced as `Open` and closed the picker; no new
      TextInputHost window and no TextInputHost foreground after 500 ms; the Esc key-down was
      traced while the picker was open in 21 of 21 Esc-closed opens.)
- [x] Win+E still opens Explorer. (Verified 2026-11-18 with the hook installed in test mode,
      keys sent with `SendInput`; the new Explorer window was closed again.)
- [ ] In a blocklisted game Win+. reaches the game (the Windows panel opens there), and the
      log (or Process Explorer) shows the hook uninstalled while the game has focus.
- [x] The picker gets keyboard focus every time across 20 Win+. opens (typing goes to the
      search field; the log has no `SetForegroundWindow refused`). (Verified 2026-11-18 in
      test mode with `SendInput`: 20/20 opens took the foreground, checked with
      `GetForegroundWindow`; no refusal logged.)
- [ ] Win+L, unlock, then type `.` in Notepad: a period is typed and no picker opens.
- [ ] Win+L, unlock, then Win+. still opens the picker (the matcher was reset, not stuck).
- [ ] Making `SetWindowsHookExW` fail (or a debugger forcing `is_lost`) shows the balloon
      "Win+. could not be captured, switching to Ctrl+Shift+Space", and `config.toml` then has
      `hotkey = "Ctrl+Shift+Space"` with tone and size unchanged.

## Inserting the emoji (Task 14)

Setup: a debug build (logs go to stderr). Each insert logs
`ui: insert "<text>" into <hwnd>: <result> in <time>`; a slow or refused foreground switch also
logs `insert: foreground first=.. attached=.. second=.. ok=..`. Paste mode is
`insert_mode = "paste"` or the exe's file name in `paste_exes`.

- [ ] Insert 👍🏽, 🏳️‍🌈, #️⃣ and a 3-item Shift queue into Notepad: the text arrives exactly.
- [ ] The same four inserts into Chrome (a text field).
- [ ] The same four inserts into Discord.
- [ ] The same four inserts into Windows Terminal.
- [ ] Discord in paste mode (`paste_exes = ["discord.exe"]`): the emoji arrives and the
      previous clipboard text is back on the clipboard afterwards.
- [ ] Paste mode with a screenshot (Win+Shift+S) on the clipboard: the emoji arrives and the
      screenshot still pastes into Paint afterwards. With rich text copied from Word or a
      browser: the formatting still pastes afterwards. With files copied in Explorer: the
      files still paste afterwards (or, if their data cannot be read, the emoji is typed and
      the log says `clipboard cannot be saved`).
- [ ] Quit from the tray right after a paste: the previous clipboard content is back.
- [ ] An admin Notepad shows the balloon "Can't type into admin windows, emoji copied to
      clipboard" and the emoji is on the clipboard.
- [ ] Close Notepad while the picker is open, then pick: the balloon "Target window is gone,
      emoji copied to clipboard" shows and the emoji is on the clipboard.
- [ ] The recents row shows the last picks (newest first) after reopening the picker and
      after restarting the app.
- [ ] After a Shift+Enter queue (Shift still held at the final pick), no modifier stays stuck
      and the typed text is unchanged.

Known issues:

- Paste mode: images on the clipboard may be restored slightly wrong (pixels shifted).
  Might get fixed in the future.

## Settings, first-run dialog and About (Task 15)

Setup: a debug build (logs go to stderr). Debug and `diag` builds read `EMOJI_PICKER_RUN_KEY`
(a key path under HKCU, for example `Software\emoji-picker-test\Run`) and write the Start with
Windows value there instead of the real Run key; without it they write the real
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value `emoji-picker`. The first run also
writes it (Start with Windows is on by default). `EMOJI_PICKER_UI_TEST` scripts settings
changes for the automated driver (see `app/src/settings.rs`).

- [ ] Delete the config and start: the first-run dialog appears. Choose each option once
      (restart with a deleted config each time): Win+. (preselected), Ctrl+Shift+Space, and
      Custom (records the next key combination; Shift+A shows an inline error, Ctrl+Alt+E
      shows the AltGr warning). Closing the dialog with X applies the selected option
      (Ctrl+Shift+Space after selecting it with Space, a valid recorded Custom key), or Win+.
      while Custom has no valid recording.
- [ ] Changing the hotkey in settings takes effect without a restart (old hotkey stops, new
      one opens the picker).
- [ ] "Add current game" lists the running apps with windows; clicking Add moves one to the
      blocklist.
- [ ] Start with Windows toggles the Run value
      (`reg query HKCU\Software\Microsoft\Windows\CurrentVersion\Run`).
- [ ] About shows the credits, the version and the numbers (last picker open in ms, private MB).
- [ ] Dark mode follows Windows in all three windows, with no white flash on open.
- [ ] All three windows work with the keyboard only: Tab moves through the controls, Up and
      Down move between the hotkey options, Space selects, Enter confirms the first-run
      dialog, Esc closes settings and About.
- [ ] Up and Down on the hotkey options only move the focus ring: the saved hotkey does not
      change (settings) and arrowing onto Custom does not start recording. Space or a click
      on Custom starts recording (when there is no recorded key yet), as does Record. Same in
      the first-run dialog, where Enter applies the selected option, not the focused one.
      (Covered headlessly by `hotkey_radio_arrows_only_move_focus`.)
- [ ] While recording, the current hotkey is off: pressing it is recorded (or rejected)
      instead of opening the picker. It works again after a recording ends by any path:
      a valid key, an error (Shift+A), Esc, switching to another window, or closing the
      settings window or the first-run dialog with X. The debug log shows
      `service: hotkey suspended=true` and then `=false`.
- [ ] Another app holding the chosen hotkey: the balloon shows and the settings hotkey field
      turns red; a hand-edited invalid hotkey (`hotkey = "A"`) is red too.
- [ ] Disable recently used emojis: the recents row disappears and `recents.txt` is deleted;
      turning it back on starts with an empty row.
- [ ] Settings, setup and About windows show the app icon in the title bar and taskbar.

Note: recording Win+. under Custom may not work while the Win+. hook is suspended, because
Windows can take Win+. for its own emoji panel. Win+. has its own option, so this only
matters for Custom.

## Installer and updates (Task 16)

These checks install the app for the current user and change the real HKCU Run value. Run
them in a Git Bash in the repo root. They need the .NET SDK and the `vpk` CLI at the same
version as the `velopack` crate in `app/Cargo.toml` (1.2.161):

```sh
export PATH="/c/Program Files/dotnet:$PATH"   # if a fresh shell does not find dotnet
# --add-source is needed when no NuGet source is configured ("No NuGet sources are defined").
dotnet tool install vpk --version 1.2.161 --add-source https://api.nuget.org/v3/index.json --tool-path .superpowers/sdd/2026-11-18-emoji-picker/tools
VPK=.superpowers/sdd/2026-11-18-emoji-picker/tools/vpk
OUT=.superpowers/sdd/2026-11-18-emoji-picker/pack-test
rm -rf "$OUT"   # start empty: a later pack overwrites Setup.exe with the newer version
```

Pack 0.0.1 and 0.0.2 with a `diag` build, so the update source can be a local folder
(`EMOJI_PICKER_UPDATE_DIR` is read only by debug and `diag` builds) and the log is on stderr.
Same flags as `release.yml`:

```sh
cargo build --release -p emoji-picker --features diag
mkdir -p target/vpk-pack && cp target/release/emoji-picker.exe target/vpk-pack/
$VPK pack --packId emoji-picker --packVersion 0.0.1 --packDir target/vpk-pack \
  --mainExe emoji-picker.exe --icon assets/generated/app.ico --outputDir "$OUT"
ls -l "$OUT"   # emoji-picker-win-Setup.exe: below 30 MB, target 15 MB (2026-11-18: 30,742,993 bytes)
```

- [ ] Run `$OUT/emoji-picker-win-Setup.exe`: it installs to
      `%LOCALAPPDATA%\emoji-picker` with no UAC (administrator) prompt and starts the app.
      Delete `%APPDATA%\emoji-picker\config.toml` first if it exists, so the first-run
      dialog appears.
- [ ] The first-run dialog appears after install; after choosing a hotkey the picker opens.
- [ ] Settings > "Check for updates now" on 0.0.1 with no newer package shows "You have the
      latest version (v0.0.1)." under the button. In a dev run (`cargo run -p emoji-picker`)
      it shows "Updates are available only in the installed app." and makes no network call.
- [ ] Update 0.0.1 to 0.0.2 from the local folder: quit the app (tray > Quit), then

      ```sh
      $VPK pack --packId emoji-picker --packVersion 0.0.2 --packDir target/vpk-pack \
        --mainExe emoji-picker.exe --icon assets/generated/app.ico --outputDir "$OUT"
      EMOJI_PICKER_UPDATE_DIR="$(cygpath -w "$OUT")" "$LOCALAPPDATA/emoji-picker/current/emoji-picker.exe"
      ```

      About 10 seconds after start the log shows `updater: downloading 0.0.2` and
      `updater: 0.0.2 downloaded`, a balloon says "Emoji picker 0.0.2 is ready", and the tray
      menu has "Restart to update (v0.0.2)" at the top. Choosing it closes the app (tray icon
      gone) and starts 0.0.2; About shows 0.0.2.
- [ ] Update on next start: repeat with 0.0.3 but quit from the tray instead of restarting.
      The next start applies 0.0.3 before the app opens (About shows 0.0.3).
- [ ] With Start with Windows on, the Run value exists
      (`reg query HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v emoji-picker`).
      Uninstall from Windows Settings > Apps: the app quits, `%LOCALAPPDATA%\emoji-picker`
      is gone and the same `reg query` reports that the value does not exist.
- [ ] Afterwards: remove `$OUT` and `target/vpk-pack`.
