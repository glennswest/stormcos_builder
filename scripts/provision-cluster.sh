#!/bin/bash
# provision-cluster.sh <name> <dns-name> <boot-method> <image-or-target>
#
# Stand up a single-node stormcos cluster on Proxmox named <name>, register
# <dns-name> in MicroDNS, boot it from the release (local disk import, or
# netboot over iSCSI / NVMe-oF/TCP), then run the control-plane bootstrap.
# Print "IP=<addr>" once the node's address is known — the service scrapes it.
#
# Wraps: terraform-modules proxmox-fedora-vm (via terragrunt) for the VM +
# DNS, stormcos-installer boot-image/qcow2 for local-disk boot, and
# stormcos-installer/scripts/bootstrap-cluster.sh for the rustkube control
# plane. Env-specific; PROXMOX_API_TOKEN must be exported.
set -euo pipefail

NAME="$1"; DNS="$2"; BOOT="$3"; IMAGE="$4"
echo "provision: name=$NAME dns=$DNS boot=$BOOT image=$IMAGE"

INSTALLER="${INSTALLER_DIR:-$HOME/projects/stormcos-installer}"

# TODO (Proxmox host reachable + PROXMOX_API_TOKEN set):
#   1. VMID=$($INSTALLER/deploy/terragrunt/free-vmid.sh)
#   2. terragrunt apply in a per-cluster unit (module proxmox-fedora-vm),
#      fedora_image -> this release's qcow2 for local-disk; or a netboot VM
#      for iscsi/nvme-tcp. MAC->IP reserved; MicroDNS registers $DNS.
#   3. Reconfigure to OVMF/UEFI + serial; qm start.
#   4. Wait for SSH, then:
#      NODE_IP=<ip> NODE_NAME=$DNS $INSTALLER/scripts/bootstrap-cluster.sh
#   5. echo "IP=<ip>"
echo "provision: (skeleton) would create VM $NAME from $IMAGE and bootstrap rustkube"
echo "IP=0.0.0.0"
