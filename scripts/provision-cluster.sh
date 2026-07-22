#!/bin/bash
# provision-cluster.sh <name> <dns-name> <boot-method> <image-or-target>
#
# Stand up a single-node stormcos node on Proxmox from a release and print
# "IP=<addr>" once it is reachable over SSH as the QE user. The build manager
# (stormcos-builder) calls this after a build so cluster-scope QA can run
# against a real booted node — replacing hand-cranked importdisk/boot cycles.
#
# boot-method:
#   local-disk  <image> is the release qcow2 (on this host) -> imported as the
#               node's disk, booted, and assimilated to local storage.
#   iscsi|nvme-tcp  <image> is a stormblock target (netboot) — not yet wired
#               here; falls through to a clear error.
#
# Access model: images bake the QE public key (stormcos keys/qe.pub) as
# `storm`'s authorized_keys with passwordless sudo, so we wait for
# `ssh -i $QE_KEY storm@<ip>` to answer — that is also the readiness signal.
#
# Environment (all have working defaults for the g8 lab):
#   PVE_SSH     ssh prefix to the Proxmox host   (default: ssh root@pve.g8.lo)
#   PVE_STORAGE Proxmox storage for the disk      (default: test-lvm-thin)
#   PVE_IMPORT  import dir on the Proxmox host     (default: /var/lib/vz/import)
#   QE_KEY      QE private key for the readiness probe (default: ~/.ssh/stormcos-qe)
#   QE_MEM_MB / QE_CORES  VM sizing               (default: 4096 / 2)
#   QE_BRIDGE   NIC bridge                         (default: vmbr0)
#   NODE_VMID   pin a VMID for <name> (else reuse-by-name / allocate)
#   NODE_IP     skip discovery and use this IP for the readiness probe
set -euo pipefail

NAME="$1"; DNS="$2"; BOOT="$3"; IMAGE="$4"
echo "provision: name=$NAME dns=$DNS boot=$BOOT image=$IMAGE"

PVE_SSH="${PVE_SSH:-ssh -o StrictHostKeyChecking=no root@pve.g8.lo}"
PVE_STORAGE="${PVE_STORAGE:-test-lvm-thin}"
PVE_IMPORT="${PVE_IMPORT:-/var/lib/vz/import}"
QE_KEY="${QE_KEY:-$HOME/.ssh/stormcos-qe}"
QE_MEM_MB="${QE_MEM_MB:-4096}"
QE_CORES="${QE_CORES:-2}"
QE_BRIDGE="${QE_BRIDGE:-vmbr0}"

if [ "$BOOT" != "local-disk" ]; then
    echo "provision: boot method '$BOOT' not yet implemented in this script" >&2
    echo "provision: (only local-disk is wired; iscsi/nvme-tcp are netboot targets)" >&2
    exit 2
fi
[ -f "$IMAGE" ] || { echo "provision: image not found: $IMAGE" >&2; exit 1; }

# --- pick a stable VMID for this node name (reuse -> in-place reprovision) ----
vmid_for_name() {
    if [ -n "${NODE_VMID:-}" ]; then echo "$NODE_VMID"; return; fi
    local existing
    existing=$($PVE_SSH "qm list 2>/dev/null | awk -v n=\"$NAME\" '\$2==n{print \$1; exit}'")
    if [ -n "$existing" ]; then echo "$existing"; return; fi
    $PVE_SSH "pvesh get /cluster/nextid"
}
VMID=$(vmid_for_name)
echo "provision: VMID=$VMID"

# --- ensure the VM exists (create once, then reprovision its disk in place) ---
if ! $PVE_SSH "qm status $VMID >/dev/null 2>&1"; then
    echo "provision: creating VM $VMID ($NAME)"
    $PVE_SSH "qm create $VMID --name '$NAME' --memory $QE_MEM_MB --cores $QE_CORES \
        --net0 virtio,bridge=$QE_BRIDGE --serial0 socket --vga serial0 \
        --scsihw virtio-scsi-single --ostype l26 --agent 1"
fi

# --- import this release's qcow2 as the node's boot disk -----------------------
BASENAME="stormcos-${NAME}.qcow2"
PVE_HOST=$(printf '%s\n' "$PVE_SSH" | awk '{print $NF}')
echo "provision: staging image to Proxmox ($PVE_HOST:$PVE_IMPORT/$BASENAME)"
scp -o StrictHostKeyChecking=no "$IMAGE" "$PVE_HOST:$PVE_IMPORT/$BASENAME"

echo "provision: stop + import fresh disk"
$PVE_SSH "set -e
qm stop $VMID 2>/dev/null || true; sleep 2
OUT=\$(qm importdisk $VMID $PVE_IMPORT/$BASENAME $PVE_STORAGE 2>&1)
NEW=\$(echo \"\$OUT\" | grep -oE '$PVE_STORAGE:[^ ,]*vm-$VMID-disk-[0-9]+' | tail -1)
[ -n \"\$NEW\" ] || { echo \"import failed: \$OUT\" >&2; exit 1; }
qm set $VMID --scsi0 \"\${NEW},aio=io_uring,discard=on,ssd=1,iothread=0,cache=none\" >/dev/null
qm set $VMID --boot order=scsi0 >/dev/null
# reclaim any now-unused disks so volumes don't pile up across rebuilds
for u in \$(qm config $VMID | awk -F: '/^unused[0-9]+:/{print \$1}'); do qm set $VMID --delete \$u 2>/dev/null || true; done
qm start $VMID"
echo "provision: booted VMID=$VMID"

# --- discover the node IP -----------------------------------------------------
node_ip() {
    if [ -n "${NODE_IP:-}" ]; then echo "$NODE_IP"; return; fi
    local ip mac
    ip=$($PVE_SSH "qm guest cmd $VMID network-get-interfaces 2>/dev/null" \
        | grep -oE '192\.168\.[0-9]+\.[0-9]+' | grep -v '^192\.168\.[0-9]*\.1$' | head -1 || true)
    if [ -z "$ip" ]; then
        mac=$($PVE_SSH "qm config $VMID | grep -oE '([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}' | head -1")
        ip=$($PVE_SSH "ip neigh show | awk -v m=\"\${mac,,}\" 'tolower(\$5)==m{print \$1; exit}'" 2>/dev/null || true)
    fi
    echo "$ip"
}

# --- wait for QE SSH (readiness) ----------------------------------------------
SSH_QE="ssh -i $QE_KEY -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -o PreferredAuthentications=publickey -o IdentitiesOnly=yes"
IP=""
for i in $(seq 1 60); do
    [ -z "$IP" ] && IP=$(node_ip)
    if [ -n "$IP" ] && $SSH_QE "storm@$IP" 'true' 2>/dev/null; then
        echo "provision: QE SSH up at $IP after ~$((i*5))s"
        break
    fi
    sleep 5
done
[ -n "$IP" ] && $SSH_QE "storm@$IP" 'true' 2>/dev/null \
    || { echo "provision: node never became reachable over QE SSH (ip='${IP:-unknown}')" >&2; exit 1; }

echo "IP=$IP"
