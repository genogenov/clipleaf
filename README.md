# clipleaf

[![CI](https://github.com/genogenov/clipleaf/actions/workflows/ci.yml/badge.svg)](https://github.com/genogenov/clipleaf/actions/workflows/ci.yml)

A lightweight, [blazing fast](BENCHMARKS.md), all-in-one clipboard manager for Linux on Wayland.

- **CLI tools** to copy and paste text, images and any other data
- **Persistent history** that keeps every copy across reboots and keeps the clipboard alive after the source app closes (works today, control interface in progress)
- **A clean but rich UI**, as a GTK GUI and a TUI, to filter, search, preview, pin and delete entries, with user themes (GUI in progress, TUI planned)
- **Text, image, file and binary support**

Inspired by [wl-clipboard](https://github.com/bugaevc/wl-clipboard).

## Roadmap

- [x] `clipleaf-core`: a hand-written Wayland protocol client: wire format, object routing, and file descriptor passing over the Unix socket (`SCM_RIGHTS`)
- [x] `clipleaf-copy`: copy text or any data to the clipboard or the primary selection, forking to the background and returning right away
- [x] `clipleaf-paste`: paste the best text type or any specific type, or list what's on offer
- [x] Zero dependencies for `clipleaf-core`, `clipleaf-copy` and `clipleaf-paste`: no libwayland, no libc crate, only Rust's standard library and hand-written bindings for the few system calls they need
- [x] At least as fast as wl-clipboard, with bounded memory for large copies ([benchmarks](BENCHMARKS.md))
- [ ] History daemon `clipleafd`
  - [x] Records every copy: the best text and image format plus file lists, without ever blocking the Wayland connection
  - [x] Persists the history across restarts and crashes: an in-memory index, an append-only log with compaction, and large payloads in their own files
  - [x] Keeps the last 100 entries and moves a repeated copy to the top instead of storing it twice
  - [x] Skips password manager copies
  - [x] Makes JPEG thumbnails of images in the background, so an entry appears before its thumbnail
  - [x] Keeps the clipboard alive: takes it over when the app you copied from exits or clears it, and at startup if it's empty
  - [x] Serves pastes from its own files with `splice`, without blocking on slow readers
  - [x] One instance per session, with a control socket and handshake
  - [ ] Control interface: list, restore, pin, delete, clear and live updates for the UIs and scripts
  - [ ] Search over the full content of every entry
  - [ ] Paste into the focused app, with the right keys in every app (Electron and terminals included)
  - [ ] Config file and systemd user service
- [ ] GUI `clipleaf-default-gui`
  - [x] Pop-up that stays running hidden and appears instantly, drawn once at startup so the first open is as fast as the rest
  - [x] Toggle command `clipleaf-default-gui-ctl` for a keybinding, which starts the GUI when it isn't running
  - [x] Closes with Esc or a click outside the pop-up
  - [x] Daemon connection status, reconnecting on its own
  - [x] Style overrides in your own `style.css`
  - [ ] History list with text previews, image thumbnails and file lists
  - [ ] Restore the picked entry
  - [ ] Paste into the focused app after picking
  - [ ] Search
  - [ ] Live updates while open
  - [ ] Pin, delete and clear
  - [ ] Full CSS theming: documented element names, live reload
- [ ] TUI: the same actions in any terminal
- [ ] `clipleafctl`: control from scripts and dmenu-style pickers (fuzzel, rofi, wofi)
- [ ] `clipleaf-copy --file`, automatic MIME type detection, several pastes at once
- [ ] Packages for the major distros

Full details are in [TODO.md](TODO.md).

## Goals

- **Fast:** as fast as possible, and at least as fast as the great `wl-copy` and `wl-paste` for the same behavior (see [BENCHMARKS.md](BENCHMARKS.md)). Every action should feel instant, in the CLI, the TUI and the GUI alike.
- **Light:** a dependency-free core, small processes, and large data kept out of memory.
- **All-in-one:** copy, paste, history and the UIs are designed and tested together.
- **Flexible and extensible, with sensible defaults:** everything a modern clipboard manager needs, with theming and settings that don't lock users into an opinionated setup, plus sensible defaults and a zero-setup first run.

Not a goal: implementing all of wl-clipboard's functionality or being command-line compatible with it.

## Requirements

A Wayland compositor with `ext_data_control_manager_v1`, such as Hyprland, sway 1.11+ or KDE Plasma.

The GUI, `clipleaf-default-gui` (in progress), also needs GTK ≥ 4.18 and gtk4-layer-shell ≥ 1.0.4, for example Arch or Debian 13 (both checked in CI); Debian 12 and Ubuntu 24.04 are too old for it. The other tools need neither.

## Install

Build with `cargo build --release`: `clipleaf-copy`, `clipleaf-paste` and `clipleafd` end up in `./target/release/`. The GUI needs the GTK headers and is built separately with `cargo build --release -p clipleaf-default-gui`. Distro packages are planned.

## Usage

### `clipleaf-copy [options] [<text>...]`

Puts text or any data on the clipboard or the primary (middle-click) selection. It returns immediately while a background process keeps the data available, so other apps, Wayland and X11 alike, can paste it until something else is copied.

| Option | Meaning |
| --- | --- |
| `<text>...` | Copy the arguments, joined with spaces. Without arguments, stdin is copied. |
| `-t`, `--type <mime>` | Offer the data under this MIME type (default: the usual text types). |
| `-p`, `--primary` | Set the primary (middle-click) selection instead of the clipboard. |
| `-f`, `--foreground` | Keep serving in the foreground instead of returning right away. |
| `--temp-dir <dir>` | Where stdin over 128 KiB is kept (default: `$TMPDIR`, then `/tmp`). |
| `--` | Treat everything after it as text, even if it looks like an option. |
| `-h`, `--help` | Show help. |

### `clipleaf-paste [options]`

Writes the clipboard or the primary selection to stdout.

| Option | Meaning |
| --- | --- |
| `-t`, `--type <mime>` | Paste exactly this MIME type. If it isn't offered, the error lists what is. |
| `-p`, `--primary` | Paste the primary selection instead of the clipboard. |
| `-l`, `--list-types` | List the offered MIME types instead of pasting. |
| `-n`, `--no-newline` | Never add a trailing newline. By default one is added only when printing text to a terminal. |
| `-h`, `--help` | Show help. |

Without `--type`, `paste` picks the best text type (`text/plain;charset=utf-8` first), then other text such as HTML, then a file list, then an image, and otherwise the first offered type.

### `clipleafd` (history daemon)

Run it once per session, for example from your compositor's autostart (`exec-once = clipleafd` in Hyprland, `exec clipleafd` in sway). It records every copy, keeps the last 100 entries (pinned ones don't count) in `$XDG_STATE_HOME/clipleaf`, and puts the latest entry back on the clipboard when the app you copied from exits or clears it, and at startup if the clipboard is empty. Only one instance runs per session. A config file, a systemd unit and the control commands for the UIs are in progress.

### `clipleaf-default-gui` (in progress)

A pop-up that stays running hidden and appears instantly on a keybinding. Bind the toggle command, which starts the GUI if it isn't running:

```
bind = SUPER, V, exec, clipleaf-default-gui-ctl toggle   # Hyprland
```

`clipleaf-default-gui-ctl [show|hide|toggle]` (no argument = toggle). Esc or a click outside the pop-up closes it. Today it shows the daemon connection status (a dot with a tooltip); the history list comes next. Styles can be overridden in `$XDG_CONFIG_HOME/clipleaf-default-gui/style.css` (or `~/.config/clipleaf-default-gui/style.css`).

### Examples

```sh
# copy an argument
./target/release/clipleaf-copy "hello world"

# or copy piped input
cat file.txt | ./target/release/clipleaf-copy

# copy an image under its MIME type
./target/release/clipleaf-copy -t image/png < screenshot.png

# copy to the primary selection (middle-click paste)
./target/release/clipleaf-copy -p "hello"

# stay in the foreground and serve until something else is copied (Ctrl+C clears the clipboard)
./target/release/clipleaf-copy -f "hello"

# keep large input on disk instead of /tmp (often RAM-backed)
./target/release/clipleaf-copy --temp-dir ~/.cache < big.iso

# copy text that would otherwise be read as a flag
./target/release/clipleaf-copy -- "--primary"

# paste to stdout
./target/release/clipleaf-paste

# list the offered types, then paste one of them
./target/release/clipleaf-paste -l
./target/release/clipleaf-paste -t image/png > shot.png

# paste the primary selection without a trailing newline
./target/release/clipleaf-paste -p -n
```

On Wayland the owner of the clipboard has to serve the data itself, which is why `copy` leaves a background process running until something else is copied.

For debug output, build without `--release`, run the binaries from `./target/debug/` instead, and pass `-f` to `copy` so the serving process keeps its stderr.

## How it works

- **Zero dependencies** for the core library, `copy` and `paste`: no libc crate, no libwayland. The history daemon and the UIs use crates where they pay off (hashing, image thumbnails, ratatui, GTK).
- **Hand-written Wayland wire protocol** over the Unix socket: message framing, object id routing, and file descriptor passing (`SCM_RIGHTS`) through hand-written `sendmsg`/`recvmsg` bindings.
- **Minimal `unsafe`**: limited to a small FFI module (`sendmsg`, `recvmsg`, `splice`, `fork`, `fcntl`, ...), each call with a `SAFETY` comment. Architecture-specific constants are checked against the kernel headers.
- **No helper processes**: `copy` and `paste` read, serve and receive the data themselves without starting other programs, which keeps small copies and pastes fast.
- **Zero-copy where the kernel allows it**: `copy` serves input over 128 KiB from its temp file with `splice`, and `paste` streams to stdout with `splice`, so large transfers never pass through user space. The pipes in between are enlarged from 64 KiB to 1 MiB so each `splice` moves more at once.
- **Bounded memory**: input over 128 KiB goes to an unnamed temp file (`O_TMPFILE`, or create + immediate unlink), so a 100 MiB copy uses ~2.3 MB of RAM and nothing is left on disk even if the process is killed.
- **A well-behaved background process**: `copy` forks only after the compositor confirms the selection, so `copy x && paste` always sees `x`. The child gets its own session, `/` as its working directory and `/dev/null` as stdio, so it survives its terminal closing and never blocks `$(copy x)` or pipes.
- **Other apps are untrusted**: MIME type names with control characters (terminal escape sequences) or invalid UTF-8 are skipped, offers are capped, and a malformed offer never stops `copy` or `paste`.

## Benchmarks

See [BENCHMARKS.md](BENCHMARKS.md).

## Platforms

Linux only. CI builds and tests on x86_64 and aarch64, and type-checks i686, armv7, powerpc64le, s390x, riscv64, loongarch64, sparc64 and x86_64 musl (architecture-specific constants such as `O_TMPFILE` and `SOL_SOCKET` differ between them).

## Layout

| Crate | Contents |
| --- | --- |
| `core/` (`clipleaf-core`) | Wayland protocol: socket + fd passing, message reader/writer, object id routing, session setup (registry, seat, data-control device) |
| `copy/` | `clipleaf-copy` |
| `paste/` | `clipleaf-paste` |
| `daemon/` | `clipleafd`: recording, storage (in-memory index + append-only log + payload files), thumbnails, restore, IPC server |
| `ipc/` (`clipleaf-ipc`) | The daemon's control protocol: framing, messages, blocking client |
| `gui/` | `clipleaf-default-gui` (GTK4 + layer-shell) and `clipleaf-default-gui-ctl` |
| `tests-common/` | Test-only: a Wayland test compositor and helpers for the integration tests |
| planned | TUI and `clipleafctl` |
| `bench/` | `bench.sh`: the benchmark in [BENCHMARKS.md](BENCHMARKS.md) |

## License

MIT. See [LICENSE](LICENSE).
