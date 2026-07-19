#!/bin/bash
# delete-cluster.sh <name>
# Tear the VM down and release its VMID + MicroDNS name. Env-specific.
set -euo pipefail
NAME="$1"
echo "delete: name=$NAME"
# TODO: terragrunt destroy (removes VM + DNS); free-vmid.sh --release <vmid>
echo "delete: (skeleton) would destroy VM $NAME and release its name/DNS"
