#!/usr/bin/env bash
# Tests packaging/release.sh against a fake `gh` that records its calls.
# Nothing is published. Run from anywhere: packaging/tests/release-test.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
release="$here/../release.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/bin" "$tmp/dist"
printf 'fake appimage' > "$tmp/dist/Ferrufus-x86_64.AppImage"
cat > "$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
# Fake gh: logs every call; FAKE_RELEASES and FAKE_TAGS list what exists.
echo "gh $*" >> "$FAKE_LOG"
case "$1 $2" in
    "release view") [[ " ${FAKE_RELEASES:-} " == *" $3 "* ]] ;;
    "api repos/"*)
        tag="${2##*/}"
        if [[ " ${FAKE_TAGS:-} " == *" $tag "* ]]; then echo 1; else echo 0; fi ;;
    "release create")
        # Assets must exist when uploaded.
        for arg in "$@"; do
            if [[ "$arg" == /* && ! -f "$arg" ]]; then echo "missing asset $arg" >&2; exit 1; fi
        done ;;
    *) ;;
esac
GH
chmod +x "$tmp/bin/gh"

export PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=owner/repo GITHUB_SHA=0123456789abcdef0123456789abcdef01234567
version="$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' "$here/../../Cargo.toml")"
failures=0

scenario() { # name, then expectations as "+pattern" (must appear) or "-pattern" (must not)
    local name="$1"; shift
    export FAKE_LOG="$tmp/$name.log"
    : > "$FAKE_LOG"
    if ! "$release" "$tmp/dist" > "$tmp/$name.out" 2>&1; then
        echo "FAIL $name: release.sh failed"; cat "$tmp/$name.out"; failures=$((failures + 1)); return
    fi
    for expectation in "$@"; do
        local pattern="${expectation:1}"
        if [[ "$expectation" == +* ]] && ! grep -qF -- "$pattern" "$FAKE_LOG"; then
            echo "FAIL $name: expected '$pattern'"; cat "$FAKE_LOG"; failures=$((failures + 1))
        elif [[ "$expectation" == -* ]] && grep -qF -- "$pattern" "$FAKE_LOG"; then
            echo "FAIL $name: unexpected '$pattern'"; cat "$FAKE_LOG"; failures=$((failures + 1))
        fi
    done
    echo "ok   $name"
}

# First merge: no releases at all.
FAKE_RELEASES="" FAKE_TAGS="" scenario first-run \
    "+gh release create continuous" "+--target $GITHUB_SHA" "-gh release delete" \
    "+gh release create v$version" "+Ferrufus-$version-x86_64.AppImage" "+--prerelease --title Ferrufus $version"

# Same version again: replace continuous only.
FAKE_RELEASES="continuous v$version" FAKE_TAGS="continuous v$version" scenario same-version \
    "+gh release delete continuous" "+gh release create continuous" "-gh release create v$version"

# Version bumped: the new tag is missing.
FAKE_RELEASES="continuous v0.0.9" FAKE_TAGS="continuous v0.0.9" scenario bumped \
    "+gh release delete continuous" "+gh release create v$version"

# Dry run changes nothing.
FAKE_RELEASES="continuous" FAKE_TAGS="continuous" DRY_RUN=1 scenario dry-run \
    "-gh release delete" "-gh release create"
grep -q "DRY RUN: gh release create v$version" "$tmp/dry-run.out" || { echo "FAIL dry-run output"; failures=$((failures + 1)); }

# A malformed commit id is refused.
if GITHUB_SHA='$(touch /tmp/x)' FAKE_LOG="$tmp/bad.log" "$release" "$tmp/dist" > /dev/null 2>&1; then
    echo "FAIL bad-sha: accepted"; failures=$((failures + 1))
else
    echo "ok   bad-sha"
fi

[ "$failures" = 0 ] && echo "All release tests passed." || { echo "$failures failure(s)"; exit 1; }
