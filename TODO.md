# Todo

- `history`
  - Daemon that watches the clipboard and saves every copy to disk (best text and image type plus file lists by default, more via settings), without blocking the Wayland connection
  - Keeps the clipboard alive after the source app exits or clears it
  - Restore any older entry to the clipboard
  - Case-insensitive search over the full content, not just previews
- Integration tests: the real binaries against a headless compositor, in CI
- TUI
  - Runs in any terminal, started from a keybinding; the user's window rules decide whether it floats
  - Search, pin, delete and restore entries, then paste into the focused app (Ctrl+Shift+V in terminals)
  - Text, image and file entries
  - The copy and paste options that make sense: primary selection, paste as plain text, pick a MIME type
  - A few styling options in TOML
- GUI with GTK4
  - Stays running hidden, so the toggle key shows it on top of other windows at once
  - Everything the TUI does, plus image thumbnails
  - Theming with CSS
- `clipleafctl`
  - Control the daemon from scripts: list, search, print, restore, pin, delete and clear entries
  - Output for dmenu-style pickers like fuzzel, rofi and wofi
- `copy`
  - `--file`: copy files the way a file manager does, so they paste into Nautilus, Dolphin or an upload dialog
  - MIME type inferred from the input
  - Several pastes at once
- Fallback to `zwlr_data_control_manager_v1` (river, Wayfire, older sway)
- Packaging for the major distros

## Nice to have

- Clipboard syncing across devices
- `paste -t text`: the best text type
- `paste -t image`: the first offered type with that prefix, like `wl-paste`
- `paste > shot.png`: pick the MIME type from the output file name
