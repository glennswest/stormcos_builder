//! Builder configuration (TOML).

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    /// Listen address, e.g. "0.0.0.0:8080".
    #[serde(default = "default_listen")]
    pub listen: String,
    /// State + logs + built boot images live under here.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// How often to poll the watched repos for updates (seconds).
    #[serde(default = "default_poll")]
    pub poll_interval_secs: u64,
    /// Auto-trigger a boot-image build when any watched component changes.
    #[serde(default)]
    pub auto_build: bool,
    /// How many releases to keep PER FLAVOR. Older ones are pruned
    /// automatically after each successful build — artifacts, build log and
    /// state record. Each release is ~6.5 GB (img + qcow2), so without this a
    /// builder with auto_build on fills its disk. 0 disables pruning.
    #[serde(default = "default_keep_releases")]
    pub keep_releases: usize,
    /// Assets: repos to watch. A change to one triggers a rebuild of every
    /// flavor that includes it.
    pub components: Vec<Component>,
    /// Flavors: named builds, each a named subset of the assets. e.g.
    /// "open-stormcos". Each flavor produces its own releases/boot images.
    pub flavors: Vec<Flavor>,
    #[serde(default)]
    pub scripts: Scripts,
    #[serde(default)]
    pub dns: Dns,
    /// When set, build-image runs the in-process Rust pipeline (calling the
    /// stormcos-install boot-image library + driving compose) instead of the
    /// `scripts.build_image` shell script.
    #[serde(default)]
    pub pipeline: Option<Pipeline>,
    /// QA: run the test suite after each build; tombstone on blocking failure.
    #[serde(default)]
    pub qa: Option<Qa>,
}

/// QA integration (stormcos_qa pulled into the builder VM).
#[derive(Clone, Debug, Deserialize)]
pub struct Qa {
    /// qa-runner binary.
    #[serde(default = "def_qa_runner")]
    pub runner: String,
    /// Checked-out stormcos_qa tests dir.
    pub tests_dir: PathBuf,
    /// File GitHub issues in owning repos on failure.
    #[serde(default)]
    pub file_issues: bool,
}

fn def_qa_runner() -> String {
    "qa-runner".into()
}

/// Inputs for the in-process Rust build pipeline (Linux build host).
#[derive(Clone, Debug, Deserialize)]
pub struct Pipeline {
    /// Pinned kernel image (vmlinuz).
    pub kernel: PathBuf,
    /// The BASE stormblock initramfs (busybox + static stormblock + LinuxBoot
    /// init). The pipeline assembles the final boot initramfs from this per
    /// build: inject the artifact's volumes.dat, then the storage modules.
    pub initramfs: PathBuf,
    /// stormcos-initramfs.sh — injects the decompressed storage/fs modules.
    pub initramfs_sh: PathBuf,
    /// A depmod'd /lib/modules/<kver> dir the module injector reads from.
    pub modules_dir: PathBuf,
    /// Kernel version string (the <kver> under modules_dir).
    pub kver: String,
    /// Optional: rebuild the base stormblock initramfs first (when stormblock
    /// changed). build-stormblock-initramfs.sh + the static stormblock binary.
    #[serde(default)]
    pub stormblock_initramfs_sh: Option<PathBuf>,
    #[serde(default)]
    pub stormblock_bin: Option<PathBuf>,
    /// systemd-bootx64.efi.
    pub bootloader: PathBuf,
    /// Guest disk device the slab partition appears as (e.g. /dev/sda).
    #[serde(default = "default_disk_device")]
    pub disk_device: String,
    /// `stormcos-compose` binary (produces the erofs rootfs + slab artifact).
    #[serde(default = "def_compose_bin")]
    pub compose_bin: String,
    /// Layers dir consumed by `compose edition` (base + driver + edition).
    pub layers_dir: PathBuf,
    /// Directory holding edition definitions (editions/<name>.toml). compose
    /// resolves it relative to CWD by default; the builder passes it absolute
    /// so it works regardless of the service's working directory.
    pub editions_dir: PathBuf,
    /// Prebuilt image-store erofs to embed as the second volume.
    pub image_store: PathBuf,
    /// `qemu-img` for the qcow2 conversion.
    #[serde(default = "def_qemu_img")]
    pub qemu_img: String,
}

fn default_disk_device() -> String {
    "/dev/sda".into()
}
fn def_compose_bin() -> String {
    "stormcos-compose".into()
}
fn def_qemu_img() -> String {
    "qemu-img".into()
}

/// A build flavor — a layer in the stack. Flavors compose upward: `extends`
/// names a base flavor, and `assets` are the components this layer adds on top.
/// So you build up base -> network -> ... -> a full release, and "special
/// versions" are just flavors that extend an existing one.
#[derive(Clone, Debug, Deserialize)]
pub struct Flavor {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Base flavor this one layers on top of (its assets are included first).
    #[serde(default)]
    pub extends: Option<String>,
    /// Component names this layer adds (on top of `extends`).
    #[serde(default)]
    pub assets: Vec<String>,
}

impl Config {
    pub fn flavor(&self, name: &str) -> Option<&Flavor> {
        self.flavors.iter().find(|f| f.name == name)
    }

    /// Fully-resolved, layer-ordered, de-duplicated asset names for a flavor
    /// (walking the `extends` chain base-first). Cycle-safe.
    pub fn flavor_asset_names(&self, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen_flavors = Vec::new();
        self.resolve_assets(name, &mut out, &mut seen_flavors);
        out
    }

    fn resolve_assets(&self, name: &str, out: &mut Vec<String>, chain: &mut Vec<String>) {
        if chain.iter().any(|n| n == name) {
            return; // cycle guard
        }
        chain.push(name.to_string());
        let Some(f) = self.flavor(name) else { return };
        if let Some(base) = &f.extends {
            self.resolve_assets(base, out, chain);
        }
        for a in &f.assets {
            if !out.contains(a) {
                out.push(a.clone());
            }
        }
    }

    /// The watched components in a flavor's resolved asset list, layer-ordered.
    pub fn flavor_components(&self, name: &str) -> Vec<&Component> {
        let names = self.flavor_asset_names(name);
        names
            .iter()
            .filter_map(|n| self.components.iter().find(|c| &c.name == n))
            .collect()
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Component {
    /// Short name (e.g. "stormblock").
    pub name: String,
    /// GitHub "owner/name".
    pub repo: String,
    /// Branch to watch.
    #[serde(default = "default_branch")]
    pub branch: String,
}

/// External scripts that do the environment-specific heavy lifting. Each is
/// invoked with well-known args and its stdout/stderr captured to a log.
/// Defaults point at the scripts shipped in this repo's `scripts/`.
#[derive(Clone, Debug, Deserialize)]
pub struct Scripts {
    /// build-image <release-id> <out-image-path>  → builds a stormcos boot image.
    #[serde(default = "def_build")]
    pub build_image: String,
    /// provision-cluster <name> <dns-name> <boot-image-path> → prints the node IP.
    #[serde(default = "def_provision")]
    pub provision_cluster: String,
    /// rebuild-machine <name> <boot-image-path> → wipe the VM disk + redeploy.
    #[serde(default = "def_rebuild")]
    pub rebuild_machine: String,
    /// delete-cluster <name> → tear the VM down and release its name/DNS.
    #[serde(default = "def_delete")]
    pub delete_cluster: String,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct Dns {
    /// MicroDNS REST base, e.g. "http://192.168.8.252:8080/api/v1".
    #[serde(default)]
    pub base_url: String,
    /// Zone id for the search domain.
    #[serde(default)]
    pub zone_id: String,
}

impl Default for Scripts {
    fn default() -> Self {
        Self {
            build_image: def_build(),
            provision_cluster: def_provision(),
            rebuild_machine: def_rebuild(),
            delete_cluster: def_delete(),
        }
    }
}

fn default_listen() -> String {
    "0.0.0.0:8080".into()
}
fn default_data_dir() -> PathBuf {
    "/var/lib/stormcos-builder".into()
}
fn default_poll() -> u64 {
    300
}
fn default_keep_releases() -> usize {
    3
}
fn default_branch() -> String {
    "main".into()
}
fn def_build() -> String {
    "scripts/build-image.sh".into()
}
fn def_provision() -> String {
    "scripts/provision-cluster.sh".into()
}
fn def_rebuild() -> String {
    "scripts/rebuild-machine.sh".into()
}
fn def_delete() -> String {
    "scripts/delete-cluster.sh".into()
}

impl Config {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        let cfg: Config = toml::from_str(&text)?;
        Ok(cfg)
    }

    pub fn images_dir(&self) -> PathBuf {
        self.data_dir.join("images")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }
    pub fn state_file(&self) -> PathBuf {
        self.data_dir.join("state.json")
    }
}
