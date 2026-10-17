# clipleaf

[![CI](https://github.com/genogenov/clipleaf/actions/workflows/ci.yml/badge.svg)](https://github.com/genogenov/clipleaf/actions/workflows/ci.yml)

A lightweight, [blazing fast](BENCHMARKS.md), all-in-one clipboard manager for Linux on Wayland.

- **CLI tools** to copy and paste text, images and any other data
- **Persistent history** that keeps every copy across reboots and keeps the clipboard alive after the source app closes (coming soon)
- **A clean but rich UI**, as a TUI and a GTK GUI, to filter, search, preview, pin and delete entries, with user themes (coming soon)
- **Text, image, file and binary support**

Inspired by [wl-clipboard](https://github.com/bugaevc/wl-clipboard).

## Roadmap

- [x] `clipleaf-core`: a hand-written Wayland protocol client: wire format, object routing, and file descriptor passing over the Unix socket (`SCM_RIGHTS`)
- [x] `clipleaf-copy`: copy text or any data to the clipboard or the primary selection, forking to the background and returning right away
- [x] `clipleaf-paste`: paste the best text type or any specific type, or list what's on offer
- [x] Zero dependencies for `clipleaf-core`, `clipleaf-copy` and `clipleaf-paste`: no libwayland, no libc crate, only Rust's standard library and hand-written bindings for the few system calls they need
- [x] At least as fast as wl-clipboard, with bounded memory for large copies ([benchmarks](BENCHMARKS.md))
- [ ] History daemon: keeps every copy (best text and image format plus file lists by default, more via settings), persists and indexes it, re-owns the clipboard when the source app exits or clears it, and offers an IPC control interface
- [ ] Search over the full content of every entry
- [ ] TUI: search, preview, pin, delete, restore, and paste into the focused app
- [ ] GUI: instant pop-up with the same actions, image thumbnails and CSS theming
- [ ] `clipleafctl`: control from scripts and dmenu-style pickers (fuzzel, rofi, wofi)
- [ ] `clipleaf-copy --file`, automatic MIME type detection, several pastes at once
- [ ] Older compositors: river, Wayfire, sway before 1.11
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

## Install

Build with `cargo build --release`. The binaries end up in `./target/release/`. Distro packages are planned.

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

Without `--type`, `paste` picks the best text type (`text/plain;charset=utf-8` first), or the first offered type if there is no text.

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

- **Zero dependencies** for the core library, `copy` and `paste`: no libc crate, no libwayland. The history daemon and the UIs use crates where they pay off (SQLite, image decoding, ratatui, GTK).
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
| planned | History daemon, TUI, GUI and `clipleafctl` |
| `bench/` | `bench.sh`: the benchmark in [BENCHMARKS.md](BENCHMARKS.md) |

## License

MIT. See [LICENSE](LICENSE).
