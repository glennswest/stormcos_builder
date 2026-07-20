#!/bin/bash
# setup-host.sh — install the OS packages the stormcos-builder pipeline shells
# out to. Run once on the builder host (the dedicated VM, or dev.g8.lo) before
# starting the service. Idempotent.
#
# What each package is for:
#   qemu-img      — raw -> qcow2 conversion (Format::Qcow2)
#   erofs-utils   — mkfs.erofs, the compose edition rootfs + image-store
#   zstd cpio xz  — initramfs assembly (unpack/repack the cpio, decompress .ko.xz)
#   busybox       — the initramfs shell + tools (/init is a busybox script). MUST
#                   be STATIC: an initramfs has no /usr/lib to link against, so a
#                   dynamic busybox fails at boot. Fedora's stock busybox is
#                   already statically linked; on Debian the package is
#                   busybox-static. Without it the base-initramfs rebuild step
#                   (stormblock_initramfs_sh) can't run.
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

PKGS="qemu-img erofs-utils zstd cpio xz busybox dosfstools git"

if command -v dnf >/dev/null; then
    dnf install -y $PKGS
elif command -v apt-get >/dev/null; then
    # Debian splits the static busybox into its own package.
    apt-get update && apt-get install -y qemu-utils erofs-utils zstd cpio xz-utils \
        busybox-static dosfstools git
else
    echo "ERROR: no dnf/apt-get — install manually: $PKGS" >&2
    exit 1
fi

echo "== verify =="
for t in qemu-img mkfs.erofs zstd cpio xz busybox; do
    printf '  %-12s ' "$t"; command -v "$t" >/dev/null && "$t" --version 2>&1 | head -1 || echo "MISSING"
done

# busybox must be static or the initramfs it lands in won't boot.
BB=$(command -v busybox || true)
if [ -n "$BB" ] && ! file "$BB" 2>/dev/null | grep -q "statically linked"; then
    echo "WARNING: $BB is NOT statically linked — the base-initramfs rebuild will" >&2
    echo "         produce an initramfs that fails at boot (no shared libs there)." >&2
fi
