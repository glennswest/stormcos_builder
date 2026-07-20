//! In-process Rust build pipeline (the `build-image` implementation).
//!
//! Replaces the shell build script: the flagship boot-artifact step runs
//! **in-process** via the `stormcos-install` boot-image library, the compose
//! steps are driven through the `stormcos-compose` binary (our Rust CLI), and
//! only genuinely external tools are shelled out (mkfs.erofs lives inside
//! compose already; qemu-img for the qcow2). The manifest is built natively.
//!
//! Everything we originate stays Rust; the pipeline links the boot-image
//! assembly directly rather than orchestrating a bash script.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::config::{Config, Pipeline};
use crate::model::{Artifact, Format, NetworkTarget, Transport};

/// Build one flavor's boot artifacts into `out_dir`, returning the artifacts
/// (img/qcow2/iso) and network boot targets for the release.
pub async fn build(
    cfg: &Config,
    flavor: &str,
    release_id: &str,
    out_dir: &Path,
    log: &Path,
) -> anyhow::Result<(Vec<Artifact>, Vec<NetworkTarget>)> {
    let p = cfg
        .pipeline
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no [pipeline] configured"))?;
    std::fs::create_dir_all(out_dir)?;
    let mut logf = LogFile::create(log)?;
    logf.line(&format!("rust pipeline: flavor={flavor} release={release_id}"));

    // 1. compose the erofs rootfs for this flavor (stormcos-compose edition).
    let ed_out = out_dir.join(format!("edition-{release_id}"));
    run(
        &p.compose_bin,
        &[
            "--out-dir".into(),
            ed_out.to_string_lossy().into(),
            "edition".into(),
            flavor_edition(flavor).into(),
            "--layers-dir".into(),
            p.layers_dir.to_string_lossy().into(),
        ],
        &mut logf,
    )
    .await?;
    let rootfs = ed_out
        .join("edition-kubernetes/rootfs-kubernetes-x86_64.erofs"); // compose output name

    // 2. wrap rootfs + image-store into a stormblock release volume (root.slab).
    let vol_out = out_dir.join(format!("vol-{release_id}"));
    run(
        &p.compose_bin,
        &[
            "--out-dir".into(),
            vol_out.to_string_lossy().into(),
            "image-volume".into(),
            "--rootfs".into(),
            rootfs.to_string_lossy().into(),
            "--image-store".into(),
            p.image_store.to_string_lossy().into(),
            "--release".into(),
            release_id.into(),
        ],
        &mut logf,
    )
    .await?;
    let slab = vol_out
        .join(format!("image-volume-{release_id}/root.slab"));

    // 3. boot image — IN-PROCESS via the stormcos-install library.
    let raw = out_dir.join(format!("{release_id}.img"));
    logf.line("boot-image: in-process (stormcos_install::bootimage)");
    let report = stormcos_install::bootimage::build(&stormcos_install::bootimage::BootImageSpec {
        kernel: p.kernel.clone(),
        initramfs: p.initramfs.clone(),
        bootloader: p.bootloader.clone(),
        slab,
        volume: format!("boot-template-stormcos-{release_id}"),
        esp_mib: 256,
        var_mib: 4096,
        containers_mib: 8192,
        disk_device: p.disk_device.clone(),
        extra_cmdline: None,
        out: raw.clone(),
    })?;
    logf.line(&format!("boot-image: {} bytes", report.image_bytes));

    // 4. qcow2 conversion (external qemu-img). ISO left to a later step.
    let qcow = out_dir.join(format!("{release_id}.qcow2"));
    run(
        &p.qemu_img,
        &[
            "convert".into(),
            "-f".into(),
            "raw".into(),
            "-O".into(),
            "qcow2".into(),
            raw.to_string_lossy().into(),
            qcow.to_string_lossy().into(),
        ],
        &mut logf,
    )
    .await?;

    // 5. artifacts + network boot targets.
    let mut artifacts = vec![artifact(Format::Img, &raw)?, artifact(Format::Qcow2, &qcow)?];
    if let Ok(a) = artifact(Format::Iso, &out_dir.join(format!("{release_id}.iso"))) {
        artifacts.push(a); // present only if an ISO step produced one
    }
    let host = local_ip();
    let targets = vec![
        NetworkTarget {
            transport: Transport::Iscsi,
            portal: format!("{host}:3260"),
            target: format!("iqn.2026.lo.g8:{release_id}"),
            volume: format!("boot-template-stormcos-{release_id}"),
        },
        NetworkTarget {
            transport: Transport::NvmeTcp,
            portal: format!("{host}:4420"),
            target: format!("nqn.2026.lo.g8:{release_id}"),
            volume: format!("boot-template-stormcos-{release_id}"),
        },
    ];
    logf.line("pipeline: done");
    Ok((artifacts, targets))
}

/// Flavors compose the same "kubernetes" edition today; a flavor selects its
/// asset subset via the layers it's built from. (Future: per-flavor editions.)
fn flavor_edition(_flavor: &str) -> &'static str {
    "kubernetes"
}

fn artifact(format: Format, path: &Path) -> anyhow::Result<Artifact> {
    let bytes = std::fs::metadata(path)
        .map_err(|e| anyhow::anyhow!("stat {}: {e}", path.display()))?
        .len();
    Ok(Artifact {
        format,
        path: path.to_path_buf(),
        bytes,
        sha256: sha256_file(path)?,
    })
}

struct LogFile(std::fs::File);
impl LogFile {
    fn create(path: &Path) -> anyhow::Result<Self> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        Ok(Self(std::fs::File::create(path)?))
    }
    fn line(&mut self, s: &str) {
        use std::io::Write;
        let _ = writeln!(self.0, "{s}");
    }
    fn file(&self) -> std::io::Result<std::fs::File> {
        self.0.try_clone()
    }
}

async fn run(cmd: &str, args: &[String], log: &mut LogFile) -> anyhow::Result<()> {
    log.line(&format!("$ {cmd} {}", args.join(" ")));
    let out = log.file()?;
    let err = out.try_clone()?;
    let status = Command::new(cmd)
        .args(args)
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .status()
        .await
        .map_err(|e| anyhow::anyhow!("spawn {cmd}: {e}"))?;
    anyhow::ensure!(status.success(), "{cmd} exited {status}");
    Ok(())
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    // Delegate to sha256sum to avoid a crypto dep; artifacts are large files.
    let out = std::process::Command::new("sha256sum").arg(path).output()?;
    anyhow::ensure!(out.status.success(), "sha256sum failed");
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string())
}

fn local_ip() -> String {
    std::process::Command::new("hostname")
        .arg("-I")
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .next()
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "127.0.0.1".into())
}

// Silence unused on non-pipeline builds.
#[allow(dead_code)]
fn _p(_: &Pipeline, _: PathBuf) {}
