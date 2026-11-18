# Emoji picker

A small native emoji picker for Windows. It lives in the notification area, opens with a
global hotkey (`Win+.` by default, replacing the Windows emoji panel) and types the chosen
emoji into the app you were using. It uses [Twemoji](https://github.com/jdecked/twemoji) art,
English search from [Emojibase](https://emojibase.dev) data, and stays out of the way of
fullscreen games.

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

## Build from source

Requires Rust (stable) on Windows.

```sh
cargo xtask assets          # download Twemoji and Emojibase, write assets/generated
cargo run -p emoji-picker
```

`cargo test --workspace` runs the tests. See `CLAUDE.md` for the full command list and the
architecture, and `TESTING.md` for the manual desktop checks.

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
