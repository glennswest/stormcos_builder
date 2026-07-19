#!/bin/bash
# rebuild-machine.sh <name> <image-or-target>
# Wipe the existing test VM's disk and redeploy the release image in place —
# the fast reprovision loop (qm stop; dd raw onto the LV; qm start). Keeps the
# same VMID/name/DNS. Env-specific (Proxmox host).
set -euo pipefail
NAME="$1"; IMAGE="$2"
echo "rebuild: name=$NAME image=$IMAGE"
# TODO: qm stop <vmid>; dd if=<raw> of=/dev/<storage>/vm-<vmid>-disk-0; qm start
echo "rebuild: (skeleton) would wipe + redeploy $IMAGE onto $NAME"
echo "IP=0.0.0.0"
