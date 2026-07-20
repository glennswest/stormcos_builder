#!/bin/bash
# setup-host.sh — install the OS packages the stormcos-builder pipeline shells
# out to. Run once on the builder host (the dedicated VM, or dev.g8.lo) before
# starting the service. Idempotent.
#
# What each package is for:
#   qemu-img      — raw -> qcow2 conversion (Format::Qcow2)
#   erofs-utils   — mkfs.erofs, the compose edition rootfs + image-store
#   zstd cpio xz  — initramfs assembly (unpack/repack the cpio, decompress .ko.xz)
#   dosfstools    — (mtools/fatfs is in-process, but keep for ISO/ESP tooling)
#   git           — sibling-repo checkouts on the build host
#
# NOT installed here (build INPUTS, produced by the build environment, not dnf):
#   - the pinned kernel + depmod'd /lib/modules  (kernel/README.md)
#   - the composed layers dir (build-base-rootfs.sh + edition layers)
#   - the image-store erofs (compose pack-store)
#   - the static stormblock musl binary + stormcos-compose/-install binaries
#   See the builder README "Build host prerequisites".
set -euo pipefail

PKGS="qemu-img erofs-utils zstd cpio xz dosfstools git"

if command -v dnf >/dev/null; then
    dnf install -y $PKGS
elif command -v apt-get >/dev/null; then
    apt-get update && apt-get install -y qemu-utils erofs-utils zstd cpio xz-utils dosfstools git
else
    echo "ERROR: no dnf/apt-get — install manually: $PKGS" >&2
    exit 1
fi

echo "== verify =="
for t in qemu-img mkfs.erofs zstd cpio xz; do
    printf '  %-12s ' "$t"; command -v "$t" >/dev/null && "$t" --version 2>&1 | head -1 || echo "MISSING"
done
