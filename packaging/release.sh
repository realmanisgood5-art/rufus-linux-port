#!/usr/bin/env bash
# Publishes the AppImage built by build-appimage.sh as GitHub releases:
#
# - `continuous`: a pre-release replaced on every run, at GITHUB_SHA.
# - `v<version>`: created once per workspace version (Cargo.toml), when
#   that tag does not exist yet. 0.x versions are marked as pre-releases.
#
# Usage: packaging/release.sh DIST_DIR
# Environment: GITHUB_REPOSITORY, GITHUB_SHA, GH_TOKEN (for gh).
# DRY_RUN=1 prints the commands that would change anything instead of
# running them.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dist="$(realpath "${1:?usage: release.sh DIST_DIR}")"
: "${GITHUB_REPOSITORY:?}" "${GITHUB_SHA:?}"
[[ "$GITHUB_SHA" =~ ^[0-9a-f]{40}$ ]] || { echo "GITHUB_SHA is not a commit id" >&2; exit 1; }

version="$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' "$repo_root/Cargo.toml")"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || {
    echo "cannot read a valid version from Cargo.toml (got '$version')" >&2
    exit 1
}
tag="v$version"
appimage="$dist/Ferrufus-x86_64.AppImage"
[ -f "$appimage" ] || { echo "$appimage is missing" >&2; exit 1; }

run() {
    if [ "${DRY_RUN:-0}" = 1 ]; then
        printf 'DRY RUN:'
        printf ' %q' "$@"
        printf '\n'
    else
        "$@"
    fi
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
short_sha="${GITHUB_SHA:0:7}"
notice="Ferrufus is an unofficial Linux port of Rufus, not affiliated with or endorsed by the Rufus project. This build is a read-only preview: it lists devices, inspects images, computes checksums and plans writes, but never writes to a device."

# Rolling pre-release.
mkdir "$work/continuous"
cp "$appimage" "$work/continuous/Ferrufus-x86_64.AppImage"
(cd "$work/continuous" && sha256sum Ferrufus-x86_64.AppImage > SHA256SUMS)
if gh release view continuous --repo "$GITHUB_REPOSITORY" > /dev/null 2>&1; then
    run gh release delete continuous --repo "$GITHUB_REPOSITORY" --yes --cleanup-tag
fi
run gh release create continuous --repo "$GITHUB_REPOSITORY" --target "$GITHUB_SHA" --prerelease \
    --title "Continuous build ($short_sha)" \
    --notes "Latest build of the default branch, at $GITHUB_SHA (version $version). $notice" \
    "$work/continuous/Ferrufus-x86_64.AppImage" "$work/continuous/SHA256SUMS"

# Versioned release, once per version.
existing="$(gh api "repos/$GITHUB_REPOSITORY/git/matching-refs/tags/$tag" \
    --jq "map(select(.ref == \"refs/tags/$tag\")) | length")"
if [ "$existing" != 0 ]; then
    echo "$tag already exists; only the continuous release was updated."
    exit 0
fi
mkdir "$work/versioned"
name="Ferrufus-$version-x86_64.AppImage"
cp "$appimage" "$work/versioned/$name"
(cd "$work/versioned" && sha256sum "$name" > SHA256SUMS)
prerelease=()
if [[ "$version" == 0.* || "$version" == *-* ]]; then
    prerelease=(--prerelease)
fi
run gh release create "$tag" --repo "$GITHUB_REPOSITORY" --target "$GITHUB_SHA" "${prerelease[@]}" \
    --title "Ferrufus $version" --notes "$notice" --generate-notes \
    "$work/versioned/$name" "$work/versioned/SHA256SUMS"
echo "Created $tag."
