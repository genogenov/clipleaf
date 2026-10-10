# Todo

- Integration tests: the real binaries against a headless compositor, in CI
- `history`
  - Daemon that watches the clipboard and saves every copy to disk, all of its MIME types, without blocking the Wayland connection
  - Keeps the clipboard alive after the source app exits or clears it
  - Restore any older entry to the clipboard
  - Case-insensitive search over the full content, not just previews
- `copy`
  - `--file`: copy files the way a file manager does, so they paste into Nautilus, Dolphin or an upload dialog
  - MIME type inferred from the input
  - Several pastes at once
- Fallback to `zwlr_data_control_manager_v1` (river, Wayfire, older sway)
- Packaging for the major distros

## Nice to have

- `paste -t text`: the best text type
- `paste -t image`: the first offered type with that prefix, like `wl-paste`
- `paste > shot.png`: pick the MIME type from the output file name
