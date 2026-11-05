#!/bin/sh
# Docked Browser, native (nanoui). GTK 3 + the CPU renderer is the lightest
# mode on this machine (~58 MB vs ~170 MB for GTK 4, which loads the GPU stack).
# DOCKED_URL overrides the server (default http://127.0.0.1:8787).
here=$(dirname "$(readlink -f "$0")")
NANOUI_GTK=${NANOUI_GTK:-3} NANOUI_RENDERER=${NANOUI_RENDERER:-cpu} exec "$here/zig-out/bin/docked_native" "$@"
