# Todo

- History daemon `clipleafd`
  - Control interface for the UIs and scripts: list, status, restore, pin, delete, clear, live updates (the connection handshake works today)
  - Case-insensitive search over the full content of every entry, file names included
  - Paste into the focused app: a virtual keyboard that uses the seat's own keymap (so Electron apps like VS Code get the right keys), focused-app tracking, Ctrl+Shift+V for terminals
  - Config file: restore on clear and on start, primary selection tracking, history size, which image types to keep
  - Clean shutdown on SIGTERM, a systemd user service, setup docs for Hyprland, sway and others
  - Password manager copies: keep them in memory only, with an expiry, masked in the UIs
  - Larger histories (tens of thousands of entries) need a leaner in-memory index
  - Thumbnails made in a separate helper process with kernel memory and time limits, so a bad image can't take the daemon down
  - WebP thumbnails
  - Tests and benchmarks: copy-to-recorded time, pastes served by the daemon, memory, compared with cursor-clip
- GUI `clipleaf-default-gui`
  - Restore the picked entry with Enter or a click
  - Paste into the focused app after picking, with a checkbox that remembers the choice
  - Search box
  - Live updates while open, daemon status
  - Pin, delete and clear
  - Theming pass: documented element names and state classes, live reload of `style.css`
  - Large preview of the original image
  - Switch between an entry's types (for example the text and the image of a browser copy, or the paths and the file list) and restore only the one shown
  - `clipleaf-default-gui-ctl quit`
- TUI
  - Runs in any terminal, started from a keybinding; the user's window rules decide whether it floats
  - Search, pin, delete and restore entries, then paste into the focused app (Ctrl+Shift+V in terminals)
  - Text, image and file entries
  - The copy and paste options that make sense: primary selection, paste as plain text, pick a MIME type
  - A few styling options in TOML
- `clipleafctl`
  - Control the daemon from scripts: list, search, print, restore, pin, delete and clear entries
  - Output for dmenu-style pickers like fuzzel, rofi and wofi
- `copy`
  - `--file`: copy files the way a file manager does, so they paste into Nautilus, Dolphin or an upload dialog
  - MIME type inferred from the input
  - Several pastes at once
- Trim the public API of `clipleaf-core` to what the tools use
- Packaging for the major distros

## Nice to have

- GUI: setting to open the pop-up centered or at the mouse pointer, and closing it with a click on any monitor
- Older compositors: fallback to `zwlr_data_control_manager_v1` (river, Wayfire, sway before 1.11)
- Clipboard syncing across devices
- `clipleafctl --json` output for scripts
- `paste -t text`: the best text type
- `paste -t image`: the first offered type with that prefix, like `wl-paste`
- `paste > shot.png`: pick the MIME type from the output file name
