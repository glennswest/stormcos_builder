#!/usr/bin/env bash
# build-entry.sh — the in-container build job, staged into a throwaway devct LXC
# by stormcos-builder (devct_build.rs) and run there. Self-contained:
#
#     pull repo -> build -> push assets (GitHub release) -> write manifest
#
# The container is destroyed after this returns; nothing here is expected to
# persist. A non-zero exit is captured by the builder and filed as an issue on
# the repo being built.
#
# Inputs (env, set by the builder):
#   REPO     owner/name of the repo to build (e.g. glennswest/stormcos)
#   FLAVOR   flavor/edition to build (e.g. kubernetes)
#   RELEASE  release id / tag to publish (e.g. kubernetes-2026-07-22-ab12cd)
#   OUT      output dir in the container (default /root/out); manifest.json here
#   REF      optional git ref to build (default: default branch HEAD)
#   GH_TOKEN GitHub token with contents:write (for the release) + the private clone
set -euo pipefail

REPO="${REPO:?REPO required}"
FLAVOR="${FLAVOR:?FLAVOR required}"
RELEASE="${RELEASE:?RELEASE required}"
OUT="${OUT:-/root/out}"
REF="${REF:-}"
mkdir -p "$OUT"
echo "build-entry: repo=$REPO flavor=$FLAVOR release=$RELEASE out=$OUT ref=${REF:-<default>}"

# --- 1. pull the repo ---------------------------------------------------------
SRC=/root/src
rm -rf "$SRC"; mkdir -p "$SRC"
clone_url="https://github.com/${REPO}.git"
[ -n "${GH_TOKEN:-}" ] && clone_url="https://x-access-token:${GH_TOKEN}@github.com/${REPO}.git"
git clone --depth 50 "$clone_url" "$SRC"
cd "$SRC"
[ -n "$REF" ] && git checkout -q "$REF"
echo "build-entry: HEAD $(git rev-parse --short HEAD)"

# --- 2. build -----------------------------------------------------------------
# The build logic is the repo's own — its canonical CI build script. This keeps
# the build definition version-controlled with the code (no persistent build
# host, no out-of-tree rebuild-ci.sh). Contract: it produces the release
# artifacts under $OUT and prints the paths.
BUILD=scripts/ci-build.sh
[ -x "$BUILD" ] || { echo "ERROR: $REPO has no executable $BUILD (the repo owns its build)" >&2; exit 3; }
FLAVOR="$FLAVOR" RELEASE="$RELEASE" OUT="$OUT" bash "$BUILD"

# --- 3. push assets: publish a GitHub release with the artifacts --------------
shopt -s nullglob
assets=("$OUT"/*.qcow2 "$OUT"/*.raw.zst "$OUT"/*.iso)
[ ${#assets[@]} -gt 0 ] || { echo "ERROR: build produced no publishable artifacts in $OUT" >&2; exit 4; }
if command -v gh >/dev/null && [ -n "${GH_TOKEN:-}" ]; then
    export GH_TOKEN
    gh release view "$RELEASE" -R "$REPO" >/dev/null 2>&1 \
        || gh release create "$RELEASE" -R "$REPO" -t "$RELEASE" -n "stormcos $FLAVOR $RELEASE" --target "$(git rev-parse HEAD)"
    gh release upload "$RELEASE" -R "$REPO" "${assets[@]}" --clobber
    echo "build-entry: uploaded ${#assets[@]} asset(s) to $REPO release $RELEASE"
else
    echo "build-entry: no gh/GH_TOKEN — skipping upload (artifacts stay in $OUT for the builder to fetch)"
fi

# --- 4. manifest the builder reads back ---------------------------------------
# Matches BuildManifest { artifacts:[{format,path,...}], targets:[...] }.
{
    printf '{ "artifacts": ['
    first=1
    for a in "${assets[@]}"; do
        case "$a" in *.qcow2) fmt=qcow2;; *.raw.zst) fmt=rawzst;; *.iso) fmt=iso;; *) fmt=img;; esac
        [ $first = 1 ] || printf ','
        printf '{"format":"%s","path":"%s"}' "$fmt" "$a"
        first=0
    done
    printf '], "targets": [] }\n'
} > "$OUT/manifest.json"
echo "build-entry: wrote $OUT/manifest.json"
