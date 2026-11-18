# Emoji picker

A small native emoji picker for Windows. It lives in the notification area, opens with a
global hotkey (`Win+.` by default, replacing the Windows emoji panel) and types the chosen
emoji into the app you were using. It uses [Twemoji](https://github.com/jdecked/twemoji) art,
English search from [Emojibase](https://emojibase.dev) data, and stays out of the way of
fullscreen games.

<img width="786" height="763" alt="image" src="https://github.com/user-attachments/assets/226ccd9f-e644-489c-be0d-367cd4b1840a" />

## Install

Download `emoji-picker-win-Setup.exe` from the
[latest release](https://github.com/The-LukeZ/emoji-picker/releases/latest) and run it. It
installs for the current user only (no administrator rights) and starts the app. Updates are
downloaded in the background from GitHub Releases: choose "Restart to update" in the tray
menu, or the update installs on the next start. Uninstall from Windows Settings > Apps.

The installer is not code signed yet, so Microsoft Defender SmartScreen may show "Windows
protected your PC". Choose "More info" and then "Run anyway" to continue.

## Usage

- Press the hotkey (`Win+.` by default) in any app, type to search, and press Enter or click
  an emoji to insert it. The first start asks which hotkey to use: `Win+.`,
  `Ctrl+Shift+Space`, or a custom combination. Change it later in Settings (tray menu).
- Right-click the tray icon for Open picker, Settings, Pause hotkey, About and Quit.
- Games: the hotkey is off while a fullscreen app is in front, and for every app in the
  blocklist. In Settings, "Add current game" lists the running apps with windows, so a game
  can be added in one click.
- Some apps get the emoji pasted instead of typed: add them to the paste list in Settings.

### Limitation: apps running as administrator

Windows does not let a normal app type into a window that runs as administrator. For those
windows the emoji is copied to the clipboard instead, and a notification tells you to paste it
with `Ctrl+V`.

## Known issues

- Paste mode: if an image was on the clipboard, it can come back with shifted pixels after the
  emoji is pasted.
- Paste mode: if Windows blocks the simulated input before `Ctrl+V` is sent, the emoji stays on
  the clipboard and the previous clipboard content is lost.

## Build from source

Requires Rust (stable) on Windows.

```sh
cargo xtask assets          # download Twemoji and Emojibase, write assets/generated
cargo run -p emoji-picker
```

Other useful commands:

```sh
cargo test --workspace                                  # unit tests
cargo clippy --workspace --all-targets -- -D warnings   # lints, as in CI
cargo build --release -p emoji-picker                   # release build in target/release
```

The workspace has three crates: `core` (platform-free logic and its tests), `app` (the
Windows app, with the Slint UI in `app/ui/`) and `xtask` (asset build tooling, not shipped).

## Contributing

Describe every user-facing change with a change file:

```sh
knope document-change
```

This writes a file to `.changeset/`. Releases are prepared from these files by
[knope](https://knope.tech): every push to `main` updates a "Release vX.Y.Z" pull request,
and merging it publishes the release.

## Credits

- Emoji art: [Twemoji](https://github.com/jdecked/twemoji), copyright Twitter, Inc. and other
  contributors, licensed under [CC-BY 4.0](https://creativecommons.org/licenses/by/4.0/).
- Emoji data and search keywords: [Emojibase](https://emojibase.dev) (MIT).
- Installer and updates: [Velopack](https://velopack.io).
- Made with [Slint](https://slint.dev).

## License

MIT, see [LICENSE](LICENSE).
