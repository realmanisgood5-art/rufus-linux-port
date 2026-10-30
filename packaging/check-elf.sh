#!/usr/bin/env bash
# Checks that release binaries only need the C runtime and an old enough
# glibc, so the AppImage runs on older distributions.
#
# Usage: packaging/check-elf.sh BINARY...
set -euo pipefail

MAX_GLIBC=2.35
ALLOWED='^(libc\.so\.6|libm\.so\.6|libgcc_s\.so\.1|ld-linux-x86-64\.so\.2)$'
status=0
for binary in "$@"; do
    needed="$(readelf --dynamic --wide "$binary" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')"
    for library in $needed; do
        if ! [[ "$library" =~ $ALLOWED ]]; then
            echo "$binary: needs $library, which is not allowed" >&2
            status=1
        fi
    done
    # Version needs flagged WEAK (std's optional symbols, such as
    # pidfd_spawnp) do not stop the binary from loading on an older glibc.
    newest="$(readelf --version-info --wide "$binary" \
        | sed -n 's/.*Name: GLIBC_\([0-9.]*\) *Flags: \(.*\) *Version.*/\1 \2/p' \
        | awk '$2 !~ /WEAK/ { print $1 }' | sort -uV | tail -n1)"
    if [ -n "$newest" ] && [ "$(printf '%s\n%s\n' "$newest" "$MAX_GLIBC" | sort -V | tail -n1)" != "$MAX_GLIBC" ]; then
        echo "$binary: needs glibc $newest, newer than $MAX_GLIBC" >&2
        status=1
    fi
    echo "$binary: needs [$(echo $needed)], glibc ${newest:-none}"
done
exit "$status"
