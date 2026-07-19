# Root Terragrunt config for stormcos-installer test infrastructure.
# Included by every unit:  include "root" { path = find_in_parent_folders("root.hcl") }
#
# Same convention as ../stormblock, ../rustkube and ../irondirectory: provider
# wiring lives here ONCE; units only declare their module `source` (always
# pinned `?ref=<tag>`) and their `inputs`.

locals {
  proxmox_endpoint = "https://pve.g8.lo:8006/"
  ssh_private_key  = pathexpand("~/.ssh/id_rsa")
}

# The API credential comes from the environment so it never lands in code or
# state. Source it from a gitignored .env:
#   export PROXMOX_API_TOKEN='terraform-svc@pve!stormcos=...'
# Must be the dedicated, pool-scoped terraform-svc@pve service credential —
# root@pam is BANNED (terraform-modules CLAUDE.md, "Incident: 2026-07-08").
generate "provider" {
  path      = "provider.tf"
  if_exists = "overwrite_terragrunt"
  contents  = <<-EOF
    provider "proxmox" {
      endpoint  = "${local.proxmox_endpoint}"
      api_token = var.proxmox_api_token
      insecure  = true
      ssh {
        agent       = false
        username    = "root"
        private_key = file("${local.ssh_private_key}")
      }
    }

    variable "proxmox_api_token" {
      type      = string
      sensitive = true
    }
  EOF
}

inputs = {
  proxmox_api_token = get_env("PROXMOX_API_TOKEN")
}
