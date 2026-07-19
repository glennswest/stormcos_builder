#!/bin/bash
# build-image.sh <flavor> <release-id> <out-dir> <manifest-path> <assets-csv>
#
# Called by stormcos-builder to build one flavor's boot image. Wraps the
# stormcos pipeline (base rootfs -> edition layer -> image-store -> erofs ->
# stormblock artifact -> boot-image) and emits every requested format
# (img/qcow2/iso), then writes a manifest the service reads to register the
# release's artifacts + network boot targets.
#
# This is the environment-specific glue; keep the heavy lifting in the
# stormcos + stormcos-installer repos and call them here. Everything below is
# a working skeleton with the real commands stubbed where they need a Linux
# build host.
set -euo pipefail

FLAVOR="$1"; RELEASE="$2"; OUTDIR="$3"; MANIFEST="$4"; ASSETS="$5"
mkdir -p "$OUTDIR"
echo "build-image: flavor=$FLAVOR release=$RELEASE assets=$ASSETS"

STORMCOS="${STORMCOS_DIR:-$HOME/projects/stormcos}"
INSTALLER="${INSTALLER_DIR:-$HOME/projects/stormcos-installer}"
KV="${KVER:-6.12.0-211.34.1.el10_2.x86_64}"

# TODO (Linux build host): drive the real pipeline for this flavor's assets:
#   1. build-base-rootfs.sh + build-edition-layer.sh (flavor's assets only)
#   2. stormcos-compose edition/image-store/pack-store/image-volume
#   3. stormcos-install boot-image  -> RAW
#   4. qemu-img convert RAW -> qcow2 ; xorriso/grub -> ISO
#   5. stormblock target export -> iSCSI IQN + NVMe NQN (for net-boot targets)
# For now, produce placeholder files so the service + UI flow works end to end.
RAW="$OUTDIR/$RELEASE.img"; QCOW="$OUTDIR/$RELEASE.qcow2"; ISO="$OUTDIR/$RELEASE.iso"
: > "$RAW"; : > "$QCOW"; : > "$ISO"

sha() { sha256sum "$1" | cut -d' ' -f1; }
sz()  { stat -c%s "$1" 2>/dev/null || stat -f%z "$1"; }
cat > "$MANIFEST" <<JSON
{
  "artifacts": [
    { "format": "img",   "path": "$RAW",  "bytes": $(sz "$RAW"),  "sha256": "$(sha "$RAW")" },
    { "format": "qcow2", "path": "$QCOW", "bytes": $(sz "$QCOW"), "sha256": "$(sha "$QCOW")" },
    { "format": "iso",   "path": "$ISO",  "bytes": $(sz "$ISO"),  "sha256": "$(sha "$ISO")" }
  ],
  "targets": [
    { "transport": "iscsi",    "portal": "$(hostname -I | awk '{print $1}'):3260", "target": "iqn.2026.lo.g8:$RELEASE", "volume": "boot-template-$RELEASE" },
    { "transport": "nvme-tcp", "portal": "$(hostname -I | awk '{print $1}'):4420", "target": "nqn.2026.lo.g8:$RELEASE", "volume": "boot-template-$RELEASE" }
  ]
}
JSON
echo "build-image: wrote $MANIFEST"
