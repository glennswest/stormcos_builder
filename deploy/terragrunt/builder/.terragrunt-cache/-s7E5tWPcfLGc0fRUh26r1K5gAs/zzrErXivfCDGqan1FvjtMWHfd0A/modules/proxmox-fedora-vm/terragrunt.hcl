# Unit: stormcos-builder — the build+provision service VM on Proxmox.
#
# PRODUCTION-CLASS, PERMANENT VM (not a throwaway/ephemeral test box): it hosts
# the web UI + REST API, builds boot images, and provisions clusters. It has a
# FIXED, reserved identity — vm_id 2059 + IP .59 (MicroDNS MAC->IP reservation,
# outside the g8 DHCP pool) + hostname stormcos-builder.g8.lo — so it is managed
# declaratively and survives rebuilds with the same address. It is NOT allocated
# a dynamic vm_id from free-vmid.sh (that pattern is for throwaway test VMs).
# The module starts it on host boot (proxmox on_boot default).
#
#   terragrunt apply        # no BUILDER_VMID env — the id is pinned below
#
# Post-provision (once the VM is up): install the pipeline's OS tools and
# stage the build inputs, then start the service:
#   ssh fedora@192.168.8.59 'sudo bash' < ../../../scripts/setup-host.sh
#   # + stage layers/, kernel modules, image-store, and the stormcos binaries
#   #   (see the builder README "Build host prerequisites"), then run
#   #   stormcos-builder against /etc/stormcos-builder.toml.

include "root" { path = find_in_parent_folders("root.hcl") }

terraform {
  source = "git::ssh://git@github.com/glennswest/terraform-modules.git//modules/proxmox-fedora-vm?ref=v0.3.0"
}

locals {
  ssh_key = trimspace(file(pathexpand("~/.ssh/id_rsa.pub")))
  node = {
    vm_id = 2059 # fixed + permanent (mnemonic to IP .59); within the module's [2000,2100]
    mac   = "BC:24:11:08:00:59"
    ip    = "192.168.8.59"
  }
}

inputs = {
  dns_zone_id        = "9bed60c8-1664-4183-88f9-a1a21b927edc" # g8.lo
  ci_ssh_public_keys = [local.ssh_key]
  tags               = ["terraform", "stormcos", "builder", "production"]
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
