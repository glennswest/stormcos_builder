//! Domain types + JSON-file persistence.
//!
//! A **release** is a built set of stormcos boot artifacts (the same image in
//! several formats: raw disk, qcow2, and bootable ISO). A **build** is one run
//! of the pipeline that produces a release. A **cluster** is an on-demand
//! single-node stormcos cluster provisioned from a release, addressed by name.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Boot artifact formats the builder emits. Same image, different container.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// Raw GPT disk image (dd to a disk / Proxmox import).
    Img,
    /// qcow2 (Proxmox / libvirt).
    Qcow2,
    /// Bootable ISO (UEFI El Torito).
    Iso,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "img" | "raw" => Some(Self::Img),
            "qcow2" | "qcow" => Some(Self::Qcow2),
            "iso" => Some(Self::Iso),
            _ => None,
        }
    }
    pub fn ext(self) -> &'static str {
        match self {
            Self::Img => "img",
            Self::Qcow2 => "qcow2",
            Self::Iso => "iso",
        }
    }
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Iso => "application/x-iso9660-image",
            _ => "application/octet-stream",
        }
    }
    pub fn all() -> [Format; 3] {
        [Self::Img, Self::Qcow2, Self::Iso]
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub format: Format,
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

/// Network boot transports: instead of downloading a disk image, a node
/// netboots and attaches the release volume as root over the network (the
/// root-on-stormblock model). The builder's stormblock server exports the
/// release volume; the node's initramfs attaches it, exports ublk root, then
/// flows over to local media. This is the PXE/diskless path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    Iscsi,
    NvmeTcp,
}

impl Transport {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "iscsi" => Some(Self::Iscsi),
            "nvmetcp" | "nvmeoftcp" | "nvme" => Some(Self::NvmeTcp),
            _ => None,
        }
    }
}

/// A network boot target the builder exports for a release.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkTarget {
    pub transport: Transport,
    /// host:port of the stormblock server exporting the volume.
    pub portal: String,
    /// iSCSI IQN or NVMe NQN identifying the target.
    pub target: String,
    /// The stormblock release volume name behind the target.
    pub volume: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Queued,
    Running,
    Success,
    Failed,
}

impl Status {
    pub fn is_terminal(self) -> bool {
        matches!(self, Status::Success | Status::Failed)
    }
}

/// The component SHAs a release was built from (for release notes / diffs).
pub type ComponentShas = BTreeMap<String, String>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
    pub id: String, // e.g. "open-stormcos-2026-07-19-a1b2c3"
    /// The flavor this release was built from.
    pub flavor: String,
    pub created: String,
    pub components: ComponentShas,
    pub notes: String,
    /// Downloadable disk-image formats.
    pub artifacts: Vec<Artifact>,
    /// Network boot targets (iSCSI / NVMe-oF/TCP) exported for this release.
    #[serde(default)]
    pub targets: Vec<NetworkTarget>,
    /// Set when a blocking QA test failed — the release is not offered for
    /// download or provisioning.
    #[serde(default)]
    pub tombstoned: bool,
    /// QA result (from qa-runner), if the pass ran.
    #[serde(default)]
    pub qa: Option<QaResult>,
}

/// Summary of a QA pass over a release (mirrors qa-runner's report).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct QaResult {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub blocking_failures: usize,
    #[serde(default)]
    pub report: Option<PathBuf>,
}

impl Release {
    pub fn artifact(&self, fmt: Format) -> Option<&Artifact> {
        self.artifacts.iter().find(|a| a.format == fmt)
    }
}

/// How a provisioned cluster gets its root: a local disk from a downloaded
/// image, or a network boot target attached over iSCSI / NVMe-oF/TCP.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BootMethod {
    /// Import the release disk image onto the VM's local disk.
    LocalDisk,
    /// Netboot + attach root over iSCSI from the builder's stormblock server.
    Iscsi,
    /// Netboot + attach root over NVMe-oF/TCP.
    NvmeTcp,
}

impl BootMethod {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "localdisk" | "local" | "disk" | "" => Some(Self::LocalDisk),
            "iscsi" => Some(Self::Iscsi),
            "nvmetcp" | "nvme" => Some(Self::NvmeTcp),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Build {
    pub id: String,
    pub status: Status,
    pub reason: String, // "manual" | "component <name> changed" | ...
    pub started: String,
    pub finished: Option<String>,
    pub log: PathBuf,
    pub release_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClusterPhase {
    Provisioning,
    Ready,
    Failed,
    Rebuilding,
    Deleting,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cluster {
    /// Short name (also the VM name); must be DNS-safe.
    pub name: String,
    /// Fully-qualified DNS name, e.g. "dev1.g8.lo".
    pub dns_name: String,
    pub phase: ClusterPhase,
    pub release_id: String,
    pub boot_method: BootMethod,
    pub ip: Option<String>,
    pub created: String,
    pub log: PathBuf,
    pub message: String,
}

/// The whole persisted state (one JSON file). Small scale — a builder tracks
/// tens of releases/clusters, not millions.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    /// Last-seen HEAD sha per watched component name.
    #[serde(default)]
    pub last_seen: BTreeMap<String, String>,
    #[serde(default)]
    pub releases: Vec<Release>,
    #[serde(default)]
    pub builds: Vec<Build>,
    #[serde(default)]
    pub clusters: Vec<Cluster>,
}

impl State {
    pub fn load(path: &std::path::Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn latest_release(&self) -> Option<&Release> {
        self.releases.last()
    }
    pub fn release(&self, id: &str) -> Option<&Release> {
        self.releases.iter().find(|r| r.id == id)
    }
    pub fn cluster(&self, name: &str) -> Option<&Cluster> {
        self.clusters.iter().find(|c| c.name == name)
    }
    pub fn cluster_mut(&mut self, name: &str) -> Option<&mut Cluster> {
        self.clusters.iter_mut().find(|c| c.name == name)
    }
    pub fn build_mut(&mut self, id: &str) -> Option<&mut Build> {
        self.builds.iter_mut().find(|b| b.id == id)
    }
}
