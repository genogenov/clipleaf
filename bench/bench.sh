#!/usr/bin/env bash
# Times clipleaf and wl-clipboard with hyperfine and prints a markdown table of medians and memory use.
set -uo pipefail

usage() {
  cat <<'EOF'
Usage: bench/bench.sh [--compositor sway|host] [--runs N]

Times copy and paste of clipleaf and wl-clipboard with hyperfine, measures
their memory and prints a markdown table of the medians. Every row is the same
behavior in both tools: copies offer the same five text types without type
detection (wl-copy gets -t text/plain for stdin), and each paste reads from the
same tool's copy.
For stable numbers pin it to fast cores, e.g. `taskset -c 0-15 bench/bench.sh`.

  --compositor sway   a private headless sway (default; needs sway 1.11+)
  --compositor host   the running compositor. This overwrites the clipboard, and a
                      clipboard manager, if any, records every benchmark copy in its history
  --runs N            runs per small case (default 100); 100 MiB cases use N/5, at least 5
EOF
}

compositor=sway
runs=100
while (($#)); do
  case $1 in
    --compositor) compositor=${2:-}; shift 2 ;;
    --runs) runs=${2:-}; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ $compositor == sway || $compositor == host ]] || { usage >&2; exit 2; }
[[ $runs =~ ^[0-9]+$ ]] && ((runs > 0)) || { usage >&2; exit 2; }
big_runs=$((runs / 5 > 5 ? runs / 5 : 5))

for tool in hyperfine wl-copy wl-paste cargo; do
  command -v "$tool" >/dev/null || { echo "$tool is required but not installed" >&2; exit 1; }
done
gnu_time=$(type -P time) || { echo "GNU time is required but not installed" >&2; exit 1; }
if [[ $compositor == sway ]] && ! command -v sway >/dev/null; then
  echo "sway is not installed: install sway 1.11+ to benchmark in a private headless compositor, or use --compositor host" >&2
  exit 1
fi

root=$(cd "$(dirname "$0")/.." && pwd)
cargo build --release --quiet --manifest-path "$root/Cargo.toml" || exit 1
C=$root/target/release/clipleaf-copy
P=$root/target/release/clipleaf-paste

W=$(mktemp -d "${TMPDIR:-/tmp}/clipleaf-bench.XXXXXX")
compositor_pid=""

start_sway() {
  mkdir -m 700 "$W/run"
  : > "$W/sway.conf"
  env -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="$W/run" \
    WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
    sway -c "$W/sway.conf" > "$W/sway.log" 2>&1 &
  compositor_pid=$!
  local socket=""
  for _ in $(seq 100); do
    socket=$(find "$W/run" -maxdepth 1 -type s -name 'wayland-*' -printf '%f\n' | head -1)
    [[ -n $socket ]] && break
    kill -0 "$compositor_pid" 2>/dev/null || break
    sleep 0.05
  done
  [[ -n $socket ]] || { echo "sway did not start:" >&2; tail -5 "$W/sway.log" >&2; exit 1; }
  export XDG_RUNTIME_DIR="$W/run" WAYLAND_DISPLAY="$socket"
  unset DISPLAY
  # The socket file appears just before sway listens on it.
  for _ in $(seq 50); do wl-copy --clear 2>/dev/null && return; sleep 0.05; done
  echo "sway is not answering" >&2
  exit 1
}

start_host() {
  [[ -n ${WAYLAND_DISPLAY:-} ]] || { echo "WAYLAND_DISPLAY is not set" >&2; exit 1; }
  echo "warning: overwriting the clipboard; a clipboard manager, if any, will record every benchmark copy" >&2
}

cleanup() {
  if [[ -n $compositor_pid ]]; then
    kill "$compositor_pid" 2>/dev/null
    wait "$compositor_pid" 2>/dev/null
  fi
  rm -rf "$W"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

if [[ $compositor == sway ]]; then start_sway; else start_host; fi

words="hello from the clipleaf bench"
printf '%s 1234567890' "$words" > "$W/small"
head -c 100M /dev/urandom > "$W/big"

# measure <id> <runs> <hyperfine options and command>: medians end up in $W/<id>.csv
measure() {
  local id=$1 n=$2
  shift 2
  echo "  $id" >&2
  hyperfine -N --style none --warmup 3 --runs "$n" --export-csv "$W/$id.csv" "$@" > /dev/null 2> "$W/$id.err" ||
    { echo "hyperfine failed for $id:" >&2; tail -3 "$W/$id.err" >&2; exit 1; }
}

median_ms() { awk -F, 'NR == 2 { printf "%.2f", $4 * 1000 }' "$W/$1.csv"; }

median() { sort -n | awk '{ a[NR] = $1 } END { print a[int((NR + 1) / 2)] }'; }

# owner_kb <tool>: resident memory in KiB of that tool's newest background copy process
owner_kb() {
  local pid
  sleep 0.1
  if [[ $1 == clipleaf ]]; then pid=$(pgrep -n -f "^$C( |$)"); else pid=$(pgrep -n -x wl-copy); fi
  [[ -n $pid ]] || { echo "no background $1 copy process found" >&2; exit 1; }
  awk '/^VmRSS:/ { print $2 }' "/proc/$pid/status"
}

# peak_kb <output> <command...>: median peak resident memory in KiB of 5 runs
peak_kb() {
  local out=$1
  shift
  for _ in 1 2 3 4 5; do "$gnu_time" -f %M -o "$W/peak" "$@" > "$out" && cat "$W/peak"; done | median
}

# measure_copy <id> <tool> <runs> <hyperfine options and command>: also records the background process's memory
measure_copy() {
  local id=$1 tool=$2
  shift 2
  measure "$id.$tool" "$@"
  owner_kb "$tool" > "$W/$id.$tool.mem"
}

# The owner keeps serving in the background, so its stderr must not be the terminal: wl-copy reports the compositor exiting.
own() {
  if [[ $1 == clipleaf ]]; then "$C" < "$2"; else wl-copy -t text/plain < "$2"; fi 2>> "$W/owner.err"
}

check_paste() {
  "$@" > "$W/out" && cmp -s "$W/out" "$W/$file" ||
    { echo "wrong paste: $* (owner $tool, $file)" >&2; tail -3 "$W/owner.err" >&2; exit 1; }
}

echo "copy..." >&2
text="'$words'"
measure_copy copy-args clipleaf "$runs" --prepare "sleep 0.05" "'$C' $text"
measure_copy copy-args wl "$runs" --prepare "sleep 0.05" "wl-copy $text"
measure_copy copy-small clipleaf "$runs" --prepare "sleep 0.05" --input "$W/small" "'$C'"
measure_copy copy-small wl "$runs" --prepare "sleep 0.05" --input "$W/small" "wl-copy -t text/plain"
measure_copy copy-big clipleaf "$big_runs" --prepare "sleep 0.1" --input "$W/big" "'$C'"
measure_copy copy-big wl "$big_runs" --prepare "sleep 0.1" --input "$W/big" "wl-copy -t text/plain"

echo "paste..." >&2
for tool in clipleaf wl; do
  if [[ $tool == clipleaf ]]; then paste_cmd=("$P" -t text/plain); else paste_cmd=(wl-paste -n -t text/plain); fi
  quoted="'${paste_cmd[0]}' ${paste_cmd[*]:1}"
  file=small
  own "$tool" "$W/small"
  check_paste "${paste_cmd[@]}"
  measure "paste-small.$tool" "$runs" "$quoted"
  peak_kb /dev/null "${paste_cmd[@]}" > "$W/paste-small.$tool.mem"
  file=big
  own "$tool" "$W/big"
  check_paste "${paste_cmd[@]}"
  measure "paste-big-null.$tool" "$big_runs" "$quoted"
  peak_kb /dev/null "${paste_cmd[@]}" > "$W/paste-big-null.$tool.mem"
  measure "paste-big-file.$tool" "$big_runs" --output "$W/out" "$quoted"
  peak_kb "$W/out" "${paste_cmd[@]}" > "$W/paste-big-file.$tool.mem"
done

case $compositor in
  sway) where="headless $(sway --version | head -1)" ;;
  host) where="${XDG_CURRENT_DESKTOP:-the host compositor}" ;;
esac
cpu=$(awk -F': ' '/model name/ { print $2; exit }' /proc/cpuinfo)

mem_mib() { awk '{ printf "%.1f", $1 / 1024 }' "$W/$1.mem"; }
# row <case> <id> <clipleaf command> <wl-clipboard command>
row() {
  printf '| %s<br>`%s`<br>`%s` | %s ms | %s ms | %s MiB | %s MiB |\n' "$1" "$3" "$4" \
    "$(median_ms "$2.clipleaf")" "$(median_ms "$2.wl")" "$(mem_mib "$2.clipleaf")" "$(mem_mib "$2.wl")"
}
echo
echo "| | clipleaf time | wl-clipboard time | clipleaf memory | wl-clipboard memory |"
echo "| --- | ---: | ---: | ---: | ---: |"
row "Copy a ${#words} B text argument" copy-args 'clipleaf-copy <text>' 'wl-copy <text>'
row 'Copy 40 B from stdin' copy-small clipleaf-copy 'wl-copy -t text/plain'
row 'Copy 100 MiB from stdin' copy-big clipleaf-copy 'wl-copy -t text/plain'
row 'Paste 40 B to `/dev/null`' paste-small 'clipleaf-paste -t text/plain' 'wl-paste -n -t text/plain'
row 'Paste 100 MiB to `/dev/null`' paste-big-null 'clipleaf-paste -t text/plain' 'wl-paste -n -t text/plain'
row 'Paste 100 MiB to a file' paste-big-file 'clipleaf-paste -t text/plain' 'wl-paste -n -t text/plain'
echo
echo "Median of $runs runs ($big_runs for 100 MiB) with $(hyperfine --version), $where, $(wl-copy --version | head -1), $cpu, Linux $(uname -r)."
echo "Memory is resident memory: for copies, of the background process that keeps the copy available; for pastes, the median peak of 5 runs (for wl-paste, the larger of wl-paste and the cat it runs)."
