---
default: patch
---

# Hotkey reliability and safer config writes

- Maximized windows no longer count as fullscreen on a monitor without a visible taskbar, so the hotkey works there.
- If Win+. cannot be captured, Ctrl+Shift+Space is used for this session only. The saved hotkey stays Win+. and the next start tries it again. Settings shows the hotkey field red with the reason. Choosing Win+. in Settings tries it again at once.
- A hotkey press while the picker is closing opens it again instead of being lost.
- A hotkey press while the picker or settings window is in front inserts into the app that was in front before.
- The config file is flushed to disk before it replaces the old one, and no `.tmp` file is left behind on failure.
- A panic in a background window callback is logged instead of closing the app.
- Known paste-mode issues are listed in the README and in Settings.
