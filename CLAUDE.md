# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Native Windows emoji picker: a tray app with a global hotkey (default `Win+.`, replacing the Windows panel) that opens a picker window and inserts the chosen emoji into the previously focused app. Rust + Slint (software renderer, winit backend), Twemoji art, Emojibase data.

## Commands

The app's build script panics if `assets/generated` is missing, so generate assets first. They are git-ignored, and the sources are pinned with SHA-256.

```sh
cargo xtask assets                                   # fetch Twemoji + Emojibase, write assets/generated/{emoji.bin,app.ico}
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p emoji-core --release -- --ignored      # search performance on real data (CI runs this)
cargo test -p emoji-core search::                    # single module / test by name filter
cargo build --release -p emoji-picker
cargo build --release -p emoji-picker --features diag  # release build with stderr logging + selftest
```

CI (`.github/workflows/ci.yml`, windows-latest) runs exactly the commands above, in that order.

## Workspace layout

- `core` (`emoji-core`): platform-free logic, fully unit-tested.
  - Emoji table and bundle format (`table.rs`, postcard; PNG bytes borrow from the embedded buffer).
  - Search, hotkey parsing, config and recents.
  - Foreground policy (blocklist and fullscreen).
  - Placement geometry and the picker state machine.
  - `WinPeriodMatcher` (`hook_matcher.rs`) for the Win+. hook.
  - Put new logic here whenever it can be tested without Windows.
- `xtask`: build tooling only (`cargo xtask` is an alias in `.cargo/config.toml`). It downloads sources, builds the table and renders the sprite atlases with resvg. It never ships.
- `app` (`emoji-picker`): the Windows binary. Slint UI in `app/ui/`, compiled from the single root `app/ui/app.slint` with the fluent style fixed. Win32 code in `app/src/win/`.

## App architecture (spans several files)

- **Two threads.**
  - The UI thread runs the Slint event loop: picker, settings and first-run windows.
  - `win/service.rs` runs a service thread. It owns a message-only window (`EmojiPickerService`), RegisterHotKey, the tray (`tray.rs`), the foreground/fullscreen gating hooks (`foreground.rs`), and session lock/unlock handling.
  - The threads talk only through `ServiceCmd` (UI to service) and `UiEvent` (service to UI). Select the Slint backend before `spawn_service`, so early events are not lost.
- **Win+. hotkey.**
  - RegisterHotKey cannot take Win+., so `ll_hook.rs` installs a WH_KEYBOARD_LL hook on its own thread, driven by the core matcher.
  - Our own injected input is tagged with `dwExtraInfo = 0x454D4F4A` and ignored by the hook.
  - If the hook fails, it falls back to Ctrl+Shift+Space.
  - `raw_input.rs` is a custom winit application handler that turns off winit's raw-input registration. With raw input on, Windows skips the LL hook while our window is in front. Keep it.
  - Never call winit `focus_window`: it sends a bare Alt.
- **Insert** (`win/insert.rs`).
  - Types the emoji with SendInput `KEYEVENTF_UNICODE`, after releasing held modifiers.
  - In paste mode (per-exe list), it snapshots every HGLOBAL clipboard format, pastes, and restores the clipboard after 300 ms.
  - Known issue: images can come back with shifted pixels (documented, not fixed).
  - Elevated or vanished targets fall back to copy plus a tray balloon.
- **Config has a single writer:** the `Picker` on the UI thread (`Picker::save_config`, `save_hotkey`, `config()`). Settings code clones `picker.config()`, edits it and saves through the Picker. Do not add a second copy of the config.
- **Picker window lifetime:** `hide()` keeps the HWND alive. Atlas images are cleared and redrawn before hiding, to keep memory down.
- Single instance via mutex `Local\emoji-picker-instance`. A second launch tells the first one to open settings.

## Debug / test hooks

Logging: the `log!` macro writes to stderr only in debug builds or with `--features diag`. Release builds are silent.

Environment variables honored in debug or `diag` builds only:
- `EMOJI_PICKER_CONFIG_DIR`: config location. Otherwise `%APPDATA%\emoji-picker\config.toml`.
- `EMOJI_PICKER_RUN_KEY`: autostart registry path. Without it, debug builds write the real HKCU Run value.
- `EMOJI_PICKER_SELFTEST=N`: opens and closes the picker N times, logs timings and memory, then quits.
- `EMOJI_PICKER_HOOK_TEST=1`: hook test mode, so injected keys are matched.

`TESTING.md` holds the manual desktop checklists for each feature.

## Rules for this repo

- The app crate has `#![deny(unsafe_op_in_unsafe_fn)]`. Put every unsafe call in its own `unsafe { }` block with a `// SAFETY:` comment.
- Ask before any test that drives the real desktop: SendInput, stealing focus, opening windows or running the app interactively. Headless `cargo test` needs no warning.
  - Avoid pressing bare modifiers to win the foreground. If a test must, document why, release every modifier, and check for stuck keys at the end.
- Use the latest stable versions when adding dependencies.
- Planning docs (specs, plans, subagent ledger) live in the git-ignored `.superpowers/`. The execution ledger is `.superpowers/sdd/2026-11-18-emoji-picker/progress.md`.
