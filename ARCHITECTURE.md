# stormcos_builder — target architecture

> This describes where the builder is going, not where it is. Today it watches
> repo commits and builds boot images in-process on a shared host from **frozen
> inputs** — which silently shipped stale images (no CRI-O, no SELinux
> enforcing, no image-store) because the staged layers/binaries never tracked
> the source. The rework below removes the shared host and the frozen inputs
> entirely. See the migration issues at the end.

## One sentence

The builder is a **reconciler**: it watches its own issue queue for "a component
build succeeded" signals, harvests the resulting **releases**, and — only when
the harvested set actually changed — builds a new stormcos image in a
**disposable LXC clone**, publishing it as a release. Nothing is staged;
everything is harvested; a build that changes nothing produces nothing.

## The two builders

There are two distinct builders, and keeping them separate is the point.

| | **component builder** | **stormcos_builder (this)** |
|---|---|---|
| Builds | one component per commit | the composed stormcos image |
| Driven by | a commit to that repo | a change in the harvested release set |
| Output | that repo's release (binaries/RPMs/images) | the stormcos release (qcow2/raw.zst + manifest) |
| Signals | **files an issue to stormcos_builder on success**, to the originator on failure | files an image-build issue on failure |

The component builder owns "build me and publish my release". stormcos_builder
owns "compose the node image from whatever released, and only when it changed".

## The event model: issues in, releases out

Coordination is **GitHub-native** — issues are the event bus, releases are the
artifact store. There is no separate queue, broker, or webhook to keep in sync.

```
commit → component builder (in an LXC clone)
    success → release  +  issue TO stormcos_builder ("build N of <repo> ok")
    failure → issue TO the originator (the author)      ← we never see this

stormcos_builder, every reconcile cycle:
    scan my open success-issues                          ← authoritative catch-up
    → harvest latest-good release of every component → manifest
    → manifest == current stormcos release's manifest?   → do nothing
    → changed?  build in an LXC clone → publish stormcos release
    → mark the processed issues (close/label)
    idle otherwise
```

Why this shape:

- **Failures never reach us.** A failed component build produces no release and
  files its issue to the *author*, not to us. We only ever hear about things
  that changed the world, so there is nothing to filter.
- **Level-triggered, not edge-triggered.** We *scan* open success-issues each
  cycle rather than relying on catching each one as it is filed. A builder that
  was restarting when three issues were filed picks all three up on the next
  scan. The issues are the durable state; scanning is authoritative.
- **Double idempotency.** A replayed or duplicate issue cannot cause a spurious
  build: (1) processed issues are closed/labeled and skipped next scan, and
  (2) if the harvested manifest is unchanged the image build is a no-op anyway.

## What triggers a stormcos build — precisely

`manifest(recipe, harvested releases) != last stormcos release's manifest`

The manifest has two kinds of input, because stormcos is both the recipe and a
component:

1. **The recipe** — the `stormcos` repo itself: `build-base-rootfs.sh` (CRI-O,
   SELinux enforcing, storm user), `editions/kubernetes.toml` (the pins), and
   the compose/install tooling. A change here changes *how* the image is built —
   e.g. adding CRI-O rebuilt the image even though no component released. This is
   exactly the case the frozen-inputs builder missed silently.
2. **The content** — the latest-good release of every harvested component.

**Best-available harvest.** If a component's newest build failed (no new
release), the image uses that component's *previous* release. One broken
component never blocks the image; it composes from last-known-good across the
board.

**Change-gated.** Identical harvested set ⇒ identical manifest ⇒ no build, no
release. Two different stormcos releases can never come from the same inputs.

**Debounced.** A burst of success-issues coalesces into one image build against
the resulting set — never one image per issue.

**No schedule.** There is no hourly/nightly image build. If nothing released,
the manifest is unchanged, so there is nothing to build.

## Builds run in disposable LXC clones (devct)

Building on a shared host is what created the frozen-inputs bug and the
87%-full-24-day-uptime box it replaces. Every build now runs in an **ephemeral
LXC clone** on Proxmox (`../devlxc`, deployed at `/opt/devct` on pve.g8.lo),
cloned copy-on-write from a golden template in ~1s and destroyed when the build
finishes. Source is cloned fresh each time — no accumulated state.

stormcos_builder is the **fleet manager** the devct README describes: it drives
`devct` (over the REST API for compile, over the pinned-command SSH path for
block), needs the scoped `builder@pve!fleet` token + restricted key from
`setup-api-token.sh`, and always destroys the clone — including on failure.

### Two templates — and which stormcos work needs which

devct ships two profiles, and stormcos build steps split across both:

| profile | template | privilege | concurrency | our work |
|---|---|---|---|---|
| **compile** | 9000 | unprivileged + nesting/keyctl/fuse | parallel | rust binaries, RPMs, container images, `mkfs.erofs`, compose, the boot-image assembly (gpt+fatfs, no root) |
| **block** | 9001 | privileged + `/dev/ublk-control` | **one at a time** | anything needing ublk/mount/modprobe/KVM — most importantly **boot-testing a produced image** (ublk root), and any step that must `mount`/`modprobe` |

The composed image build is mostly compile-profile work (dnf `--installroot`,
`mkfs.erofs --file-contexts`, initramfs assembly, gpt/fatfs boot-image — none of
which need ublk). What genuinely needs the **block** profile is **booting the
image to test it** (the node's root is ublk) and any storage step that mounts or
modprobes. So the pipeline is: build the image in a **compile** clone, then take
a **block** clone to boot-test it.

**Block is a single global slot.** `/dev/ublkb*` is host-global with no
per-container isolation, so devct serialises block builds and returns non-zero
when the slot is taken. The fleet manager must **queue block work** and back off
on contention — the stormcos boot-test and a stormblock block build compete for
the same slot.

## Provenance: build id, manifest, release notes (builder#1)

Every build carries provenance, and because the builder now *harvests* releases
it knows the exact version of everything it used, so this is a byproduct:

- **Ever-increasing build id** — a monotonic counter, allocated *before* the
  build (it names the artifacts) and recorded even on failure, so the sequence
  has no gaps hiding a failed build.
- **Version manifest (JSON)** — the resolved tag/sha of every harvested
  component. This *is* the change-detection key: "did the manifest change?" ==
  "should we build?". It is also what platform-version-operator needs to know a
  crate's contents (stormcos#17) — shared schema.
- **Release notes** — per-repo commits since the last build that used that repo,
  plus the manifest table, attached to the stormcos release alongside the
  qcow2/raw.zst.

## What the builder still does (unchanged)

- Serves images for download (qcow2 for VMs/boot-test, raw.zst for bare-metal
  `dd` — never the uncompressed raw).
- Provisions stormcos VMs from a release (`provision-stormcos-vm.sh`: import the
  qcow2 as the boot disk, OVMF/serial, virtio-scsi + io_uring for ublk root).
- Retention: keep N stormcos releases; prune older artifacts + intermediates.

## Migration from the current builder

This is a real rework, tracked as issues:

- builder#1 — build id, version manifest, release notes, provenance
- (to file) reconciler: watch own issue queue; scan-based catch-up; idempotent
  processed-issue marking
- (to file) devct fleet manager: drive compile (API) + block (SSH); token/key
  setup; always-destroy; block-slot queue
- (to file) harvest from releases (stormcos#16): edition composed from harvested
  RPMs/archives, not from binaries built on a shared host
- (to file) manifest-gated image build: build only when `manifest != last`
- retire the frozen `/opt/stormcos-builder/{inputs,src}` staging and the
  in-process shared-host build once the above land

Related: stormcos#15 (OCI-archive preload), stormcos#16 (releases as source of
truth), stormcos#17 (crate = OS+containers), stormcos#18 (image store invisible
to CRI-O), and the component builder in `../devlxc`.
