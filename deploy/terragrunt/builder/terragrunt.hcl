# Unit: stormcos-builder — the build+provision service VM on Proxmox.
# Long-lived (not throwaway): hosts the web UI + REST API, builds boot images,
# and provisions clusters. Reserved IP .59 (outside the g8 DHCP pool).
#
#   VMID=$(../free-vmid.sh) BUILDER_VMID=$VMID terragrunt apply

include "root" { path = find_in_parent_folders("root.hcl") }

terraform {
  source = "git::ssh://git@github.com/glennswest/terraform-modules.git//modules/proxmox-fedora-vm?ref=v0.3.0"
}

locals {
  ssh_key = trimspace(file(pathexpand("~/.ssh/id_rsa.pub")))
  node = {
    vm_id = tonumber(get_env("BUILDER_VMID", "0"))
    mac   = "BC:24:11:08:00:59"
    ip    = "192.168.8.59"
  }
}

inputs = {
  dns_zone_id        = "9bed60c8-1664-4183-88f9-a1a21b927edc" # g8.lo
  ci_ssh_public_keys = [local.ssh_key]
  tags               = ["terraform", "stormcos", "builder"]
  vm_datastore       = "test-lvm-thin"
  snippet_datastore  = "terraform-snippets"

  vms = {
    stormcos-builder = {
      vm_id     = local.node.vm_id
      mac       = local.node.mac
      ip        = local.node.ip
      cores     = 4
      memory    = 8192
      disk_size = 200   # room for many boot images (img+qcow2+iso per release)
    }
  }
}
