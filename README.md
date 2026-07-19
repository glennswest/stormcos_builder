# stormcos_builder

**Builds stormcos boot images and hands out clusters.** It watches the storm
component repos, rebuilds a flavor's boot image when one of its assets changes,
auto-writes release notes, serves the images for download, and provisions or
wipes-and-rebuilds single-node clusters on demand — over a **web UI** and a
**REST API**.

Not a registry — it delivers **boot images**: raw disk (`img`), `qcow2`, and
bootable `iso`, plus network **boot targets** over **iSCSI** and
**NVMe-oF/TCP** (the root-on-stormblock netboot path). Everything storm-native
and Rust (axum).

## Flavors (layer up to a release)

A flavor is a layer. Flavors compose upward via `extends`, so you build up
`base → network → … → open-stormcos`, verifying each layer before adding the
next; "special versions" are just flavors that extend an existing one.

```
base           stormcos + stormblock + rspacefs + installer
 └─ network    + CNI / networking
     └─ open-stormcos   + rustkube + rustkube-node + fastetcd + cadvisor + ironprom
```

A change to any asset rebuilds every flavor that includes it (layered
resolution). Release notes are generated from each asset's commits since the
last build.

## REST API

| Method | Path | |
|---|---|---|
| GET | `/api/v1/flavors` · `/components` | list flavors (resolved assets) / watched repos |
| POST | `/api/v1/flavors/{flavor}/build` | build a flavor's boot image now |
| GET | `/api/v1/builds` · `/builds/{id}` | build status + logs |
| GET | `/api/v1/releases[?flavor=]` · `/releases/{id}` | built releases (artifacts + net-boot targets) |
| GET | `/api/v1/releases/{id}/download/{img\|qcow2\|iso}` | stream a boot image |
| GET | `/api/v1/clusters` | list clusters |
| POST | `/api/v1/clusters` | provision one — `{name, dns_name?, flavor?, release_id?, boot_method?}` |
| GET | `/api/v1/clusters/{name}` | status |
| POST | `/api/v1/clusters/{name}/rebuild` | **wipe + rebuild** the test machine in place |
| DELETE | `/api/v1/clusters/{name}` | tear down + release the name/DNS |

`boot_method` = `local-disk` (default) · `iscsi` · `nvme-tcp`.

```bash
curl -XPOST host:8080/api/v1/clusters -H content-type:application/json \
  -d '{"name":"dev1","boot_method":"iscsi"}'
```

The web UI at `/` drives the same API: build flavors, download images,
create/rebuild/delete named clusters.

## Design

The service owns HTTP + state (a JSON file) + the watcher/job engine. The
environment-specific heavy lifting lives in `scripts/` (build the image via the
stormcos pipeline, provision/rebuild/delete via terragrunt + stormcos-installer
+ Proxmox). Each script has a well-known contract; the build script writes a
manifest the service registers. Runs as a VM on Proxmox (`deploy/terragrunt/`).

## Status

Early. Service + API + web UI + watcher + flavor layering compile and run; the
`scripts/` are working skeletons wired to the real stormcos/stormcos-installer
tooling — fill them in on the Linux build host.
