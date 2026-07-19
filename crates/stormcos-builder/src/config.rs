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
