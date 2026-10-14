# Benchmarks

These benchmarks check that clipleaf is at least as fast as the great wl-clipboard, which is already plenty fast for all practical intents and purposes.

Median times and memory measured with [`bench/bench.sh`](bench/bench.sh), which compares clipleaf directly against wl-clipboard by running both with [hyperfine](https://github.com/sharkdp/hyperfine) against a private headless sway. Every row is the same behavior in both tools: copies offer the same five text types without type detection, and each paste reads from the same tool's copy. Payloads up to 1 MiB are random base64 text; the 100 MiB payload is random bytes. 128 KiB is clipleaf's threshold between holding a copy in memory and spilling it to a temporary file, so the 1 MiB and 100 MiB rows take the file path.

| | clipleaf time | wl-clipboard time | clipleaf memory | wl-clipboard memory |
| --- | ---: | ---: | ---: | ---: |
| Copy a 29 B text argument<br>`clipleaf-copy <text>`<br>`wl-copy <text>` | 2.23 ms | 2.55 ms | 0.9 MiB | 0.6 MiB |
| Copy 40 B from stdin<br>`clipleaf-copy`<br>`wl-copy -t text/plain` | 2.41 ms | 4.28 ms | 0.9 MiB | 0.6 MiB |
| Copy 100 B from stdin<br>`clipleaf-copy`<br>`wl-copy -t text/plain` | 2.32 ms | 4.29 ms | 1.0 MiB | 0.6 MiB |
| Copy 1 KiB from stdin<br>`clipleaf-copy`<br>`wl-copy -t text/plain` | 2.38 ms | 4.36 ms | 0.9 MiB | 0.6 MiB |
| Copy 1 MiB from stdin<br>`clipleaf-copy`<br>`wl-copy -t text/plain` | 3.23 ms | 4.75 ms | 1.0 MiB | 0.6 MiB |
| Copy 100 MiB from stdin<br>`clipleaf-copy`<br>`wl-copy -t text/plain` | 29.55 ms | 32.70 ms | 1.0 MiB | 0.6 MiB |
| Paste 40 B to `/dev/null`<br>`clipleaf-paste -t text/plain`<br>`wl-paste -n -t text/plain` | 0.70 ms | 1.68 ms | 1.9 MiB | 3.6 MiB |
| Paste 100 B to `/dev/null`<br>`clipleaf-paste -t text/plain`<br>`wl-paste -n -t text/plain` | 0.66 ms | 1.22 ms | 1.9 MiB | 3.6 MiB |
| Paste 1 KiB to `/dev/null`<br>`clipleaf-paste -t text/plain`<br>`wl-paste -n -t text/plain` | 0.72 ms | 0.76 ms | 1.9 MiB | 3.6 MiB |
| Paste 1 MiB to `/dev/null`<br>`clipleaf-paste -t text/plain`<br>`wl-paste -n -t text/plain` | 0.75 ms | 1.52 ms | 1.9 MiB | 3.6 MiB |
| Paste 100 MiB to `/dev/null`<br>`clipleaf-paste -t text/plain`<br>`wl-paste -n -t text/plain` | 1.76 ms | 4.69 ms | 1.9 MiB | 3.6 MiB |
| Paste 100 B to a file<br>`clipleaf-paste -t text/plain`<br>`wl-paste -n -t text/plain` | 0.66 ms | 0.67 ms | 2.1 MiB | 3.6 MiB |
| Paste 1 KiB to a file<br>`clipleaf-paste -t text/plain`<br>`wl-paste -n -t text/plain` | 0.69 ms | 1.45 ms | 2.2 MiB | 3.6 MiB |
| Paste 1 MiB to a file<br>`clipleaf-paste -t text/plain`<br>`wl-paste -n -t text/plain` | 1.06 ms | 2.12 ms | 1.9 MiB | 3.6 MiB |
| Paste 100 MiB to a file<br>`clipleaf-paste -t text/plain`<br>`wl-paste -n -t text/plain` | 20.84 ms | 22.91 ms | 1.9 MiB | 3.6 MiB |

Memory is resident memory (RSS). For copies it is the background process that keeps the copy available until the next copy; for pastes it is the median peak of 5 runs, and for wl-paste, which runs `cat`, the larger of the two processes.

100 runs per case (20 for 100 MiB) on an Intel Core i9-14900HX pinned to its performance cores, Linux 7.2.8, sway 1.12, wl-clipboard 2.3.0, hyperfine 1.20.0.

To reproduce, install sway 1.11+, wl-clipboard, hyperfine and GNU time, then run `bench/bench.sh`, pinned to fast cores with `taskset` on hybrid CPUs (`taskset -c 0-15 bench/bench.sh` here). `bench/bench.sh --help` lists the options, including `--compositor host` to use the running compositor.
