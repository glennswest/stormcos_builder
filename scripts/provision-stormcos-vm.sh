#!/bin/bash
# provision-stormcos-vm.sh — create ONE stormcos VM on Proxmox from a release
# qcow2 and boot it. The standard-platform VM shape.
#
# stormcos is a PRE-BUILT bootable image — the qcow2 IS the disk. This is the
# opposite of the Fedora cloud-image path (blank disk + cloud-init), so it does
# not use the proxmox-fedora-vm module; it imports the image as the boot disk.
#
# The VM config mirrors the shape proven on vmid 2010: OVMF/UEFI (systemd-boot),
# an EFI disk, a serial socket (the node's console), virtio-scsi-single with
# io_uring (ublk root needs it), and boot order scsi0.
#
# Usage:
#   provision-stormcos-vm.sh <vmid> <name> <mac> <qcow2> [memory_mb] [cores]
#
# Run where `qm` is available (the Proxmox host), with the qcow2 reachable.
set -euo pipefail

VMID="${1:?vmid}"
NAME="${2:?name}"
MAC="${3:?mac BC:24:11:...}"
QCOW="${4:?path to release qcow2}"
MEM="${5:-4096}"
CORES="${6:-2}"
STORE="${STORE:-test-lvm-thin}"
BRIDGE="${BRIDGE:-vmbr0}"

[ -f "$QCOW" ] || { echo "ERROR: qcow2 not found: $QCOW" >&2; exit 1; }

if qm status "$VMID" >/dev/null 2>&1; then
    echo "VM $VMID exists — destroying for a clean provision"
    qm stop "$VMID" 2>/dev/null || true
    sleep 2
    qm destroy "$VMID" --purge 2>/dev/null || true
fi

echo "== create $VMID ($NAME) =="
qm create "$VMID" \
    --name "$NAME" \
    --memory "$MEM" \
    --cores "$CORES" \
    --cpu host \
    --ostype l26 \
    --machine q35 \
    --bios ovmf \
    --scsihw virtio-scsi-single \
    --agent enabled=1 \
    --serial0 socket \
    --net0 "virtio=$MAC,bridge=$BRIDGE" \
    --tags "stormcos,platform,master"

echo "== EFI disk (pre-enrolled-keys off; we ship our own systemd-boot) =="
qm set "$VMID" --efidisk0 "$STORE:0,efitype=4m,pre-enrolled-keys=0"

echo "== import the release image as the boot disk =="
qm importdisk "$VMID" "$QCOW" "$STORE" >/dev/null
# importdisk lands it as an unused disk — attach it as scsi0 with the flags the
# ublk root path needs (aio=io_uring, discard, ssd).
UNUSED=$(qm config "$VMID" | sed -n 's/^unused[0-9]*: //p' | head -1)
[ -n "$UNUSED" ] || { echo "ERROR: import produced no disk" >&2; exit 1; }
qm set "$VMID" --scsi0 "$UNUSED,aio=io_uring,discard=on,ssd=1"
qm set "$VMID" --boot "order=scsi0"

echo "== start =="
qm start "$VMID"
echo "STARTED $VMID $NAME $MAC"
