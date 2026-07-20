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
            // Writable per-node state as stormblock THIN volumes (grow their
            // backing independently; auto-expand on pressure) — not disk
            // partitions (which only grow the last one).
            "--var-gib".into(),
            "8".into(),
            "--containers-gib".into(),
            "20".into(),
        ],
        &mut logf,
    )
    .await?;
    let art_dir = vol_out.join(format!("image-volume-{release_id}"));
    let slab = art_dir.join("root.slab");

    // 3. assemble THIS build's boot initramfs (parity with rebuild-ci.sh):
    //    optional base rebuild -> inject the artifact's volumes.dat so the node
    //    restores its volumes by name -> inject the storage modules.
    let initramfs = assemble_initramfs(p, &art_dir, out_dir, release_id, &mut logf).await?;

    // 4. boot image — IN-PROCESS via the stormcos-install library.
    let raw = out_dir.join(format!("{release_id}.img"));
    logf.line("boot-image: in-process (stormcos_install::bootimage)");
    let report = stormcos_install::bootimage::build(&stormcos_install::bootimage::BootImageSpec {
        kernel: p.kernel.clone(),
        initramfs,
        bootloader: p.bootloader.clone(),
        slab,
        volume: format!("boot-template-stormcos-{release_id}"),
        esp_mib: 256,
        // Thin /var + /var/lib/containers volumes (names match compose's
        // var_volume_name / containers_volume_name); the initramfs exports and
        // systemd mounts them over the read-only erofs root.
        writable: vec![
            stormcos_install::bootimage::WritableMount {
                volume: format!("var-stormcos-{release_id}"),
                mount: "/var".into(),
            },
            stormcos_install::bootimage::WritableMount {
                volume: format!("containers-stormcos-{release_id}"),
                mount: "/var/lib/containers".into(),
            },
        ],
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

/// Assemble this build's boot initramfs — the same three moves rebuild-ci.sh
/// makes so the produced image actually boots:
///   0 (optional) rebuild the base stormblock initramfs when a new stormblock
///     binary is configured (its boot-local + LinuxBoot init change per build);
///   3a inject the artifact's meta/volumes.dat under etc/stormblock/meta so the
///      node restores THIS build's template + writable volumes by name;
///   3b inject the decompressed storage/fs modules (stormcos-initramfs.sh).
async fn assemble_initramfs(
    p: &Pipeline,
    art_dir: &Path,
    out_dir: &Path,
    release_id: &str,
    logf: &mut LogFile,
) -> anyhow::Result<PathBuf> {
    // 0. optional base rebuild.
    let base = match (&p.stormblock_initramfs_sh, &p.stormblock_bin) {
        (Some(sh), Some(bin)) => {
            let base = out_dir.join(format!("base-initramfs-{release_id}.img"));
            logf.line("initramfs: rebuild base (stormblock changed)");
            run(
                "bash",
                &[
                    sh.to_string_lossy().into(),
                    bin.to_string_lossy().into(),
                    p.kver.clone(),
                    base.to_string_lossy().into(),
                ],
                logf,
            )
            .await?;
            base
        }
        _ => p.initramfs.clone(),
    };

    // 3a. inject the artifact's volumes.dat into a copy of the base cpio.
    let staged = out_dir.join(format!("initramfs-meta-{release_id}.img"));
    let volumes_dat = art_dir.join("meta/volumes.dat");
    anyhow::ensure!(
        volumes_dat.is_file(),
        "artifact meta missing: {}",
        volumes_dat.display()
    );
    logf.line("initramfs: inject artifact meta (volumes.dat)");
    // busybox-compatible: unpack, drop volumes.dat under etc/stormblock/meta,
    // repack. Done via a shell one-liner so it matches rebuild-ci exactly.
    let work = out_dir.join(format!("irfs-{release_id}"));
    let script = format!(
        "set -e; rm -rf {w}; mkdir -p {w}; cd {w}; \
         zstd -dc {base} | cpio -idm --quiet; \
         mkdir -p etc/stormblock/meta; cp {vd} etc/stormblock/meta/; \
         find . | cpio -o -H newc --quiet | zstd -19 -T0 -q > {out}",
        w = work.to_string_lossy(),
        base = base.to_string_lossy(),
        vd = volumes_dat.to_string_lossy(),
        out = staged.to_string_lossy(),
    );
    run("bash", &["-c".into(), script], logf).await?;

    // 3b. inject storage/fs modules -> final initramfs.
    let final_img = out_dir.join(format!("initramfs-{release_id}.img"));
    logf.line("initramfs: inject storage modules");
    run(
        "bash",
        &[
            p.initramfs_sh.to_string_lossy().into(),
            staged.to_string_lossy().into(),
            p.kver.clone(),
            p.modules_dir.to_string_lossy().into(),
            final_img.to_string_lossy().into(),
        ],
        logf,
    )
    .await?;
    Ok(final_img)
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
