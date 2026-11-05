# Docked Browser — native (nanoui)

The dashboard at http://127.0.0.1:8787/, rebuilt as a native window with
[nanoui](https://github.com/justrach/nanoui) (Zig 0.17). It is only a new
front end: it talks to the same `../gui/server.py` JSON API and polls on the
same schedule as the web page (`/api/status` every 4 s, `/api/stats` every 10 s).

Covers: name box + Open, the Chrome-update banner (Install update), Open now
(status, CPU/RAM, Pause/Resume, Close), Saved (Open), custom icons.
Not ported: the Advanced panel, Help & about, theme switch, icon upload, log.

    zig build -Doptimize=ReleaseSmall   # needs Zig 0.17.1
    zig build test                      # offscreen, canned data, no network
    ./run.sh                            # server.py must be running

## Benchmark (2026-11-05, Fedora 44, Wayland, NVIDIA; window 680×900, idle)

| Front end                                   | Memory (PSS)          | Idle CPU   |
|---------------------------------------------|-----------------------|------------|
| Web UI as a tab in your already-open Chrome  | ~24 MB + 11 MB swapped (Chrome says 41.7 MB) | ~0.04 % |
| Web UI in its own Chrome (nothing else open) | ~449 MB (10 processes) | —          |
| **nanoui, GTK 3 + CPU renderer (`run.sh`)**  | **~58 MB**            | 0.2–0.4 %  |
| nanoui, GTK 3 + OpenGL                        | ~200 MB               | —          |
| nanoui, GTK 4 (default)                       | ~170 MB               | 0.7–0.8 %  |

Binary: 1.2 MB (ReleaseSmall). The Python server itself: ~9 MB PSS either way.
