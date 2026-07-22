//! Run the image build in an ephemeral, throwaway devct LXC instead of on a
//! persistent host. The container is a self-contained job: pull repo → build →
//! push assets → delete. The ublk/stormblock slab assembly requires the **block**
//! profile, which devct serializes to one global slot.
//!
//! The build is the builder's job in Rust: the container runs the SAME builder
//! binary (`stormcos-builder build ...`, the in-process pipeline), not a shell
//! script.
//!
//! Flow:
//!   1. create the container (devct-driver; block => serialized/backed off).
//!   2. wait for sshd, stage the builder binary + config, run `... build ...`.
//!   3. copy the manifest it produced back and parse it into the release.
//!   4. ALWAYS destroy the container (even on failure).
//!
//! A failure at any step surfaces as `Err` — the caller files an issue on the
//! repo being built (config::Devct::repo).

use std::path::Path;

use crate::config::{Config, Devct as DevctCfg};
use crate::jobs::BuildManifest;
use crate::model::{Artifact, NetworkTarget};

/// Build `flavor`/`release_id` inside a fresh devct container and return the
/// published artifacts + network targets. Errors carry enough context for the
/// caller's failure issue; the container is always destroyed.
pub async fn build(
    cfg: &Config,
    dc: &DevctCfg,
    flavor: &str,
    release_id: &str,
    out_dir: &Path,
    log: &Path,
) -> anyhow::Result<(Vec<Artifact>, Vec<NetworkTarget>)> {
    let token = dc.token();
    anyhow::ensure!(
        !token.is_empty(),
        "devct: no API token (set [devct].token or $DEVCT_TOKEN/$PROXMOX_API_TOKEN)"
    );
    let devct = devct_driver::Devct::new(&dc.api, &dc.node, &dc.ssh_host, &token)?;

    // Component asset shas -> release notes are computed by the caller; the
    // container publishes the assets it builds, so here we only orchestrate.
    let project = dc
        .repo
        .rsplit('/')
        .next()
        .unwrap_or(&dc.repo)
        .to_string();

    // 1. create (block => devct serializes; a busy slot returns Err to back off).
    let ct = devct
        .create(&project, &dc.profile, dc.cores, dc.memory_mb)
        .await
        .map_err(|e| anyhow::anyhow!("devct create ({}) for {}: {e}", dc.profile, dc.repo))?;

    // Everything from here must run the container destroy on the way out.
    let result = run_in_container(dc, &ct, flavor, release_id, out_dir, log).await;

    // 4. always destroy.
    if let Err(e) = devct.destroy(&ct).await {
        tracing::warn!("devct destroy CT {} ({}): {e}", ct.vmid, dc.profile);
    }
    let _ = cfg; // reserved for future manifest post-processing hooks
    result
}

async fn run_in_container(
    dc: &DevctCfg,
    ct: &devct_driver::Container,
    flavor: &str,
    release_id: &str,
    out_dir: &Path,
    log: &Path,
) -> anyhow::Result<(Vec<Artifact>, Vec<NetworkTarget>)> {
    let host = format!("root@{}", ct.ip);
    let ssh = |args: &[&str]| {
        let mut c = tokio::process::Command::new("ssh");
        c.args([
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "ConnectTimeout=10",
            &host,
        ]);
        c.args(args);
        c
    };

    // 2a. wait for sshd — the container gets its DHCP lease before sshd is
    // listening, so scp/ssh immediately after create races and is refused.
    let mut ready = false;
    for _ in 0..60 {
        if ssh(&["true"])
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    anyhow::ensure!(ready, "CT {} sshd never came up ({})", ct.vmid, ct.ip);

    // 2b. stage the builder binary + its container config. The build is the
    // builder's job in Rust — the container runs `stormcos-builder build ...`
    // (the same pipeline the service uses), not a shell script.
    let builder_bin = match &dc.builder_bin {
        Some(p) => p.clone(),
        None => std::env::current_exe()?,
    };
    let scp_to = |src: String, dst: &str| {
        let dst = format!("{host}:{dst}");
        let mut c = tokio::process::Command::new("scp");
        c.args([
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            &src,
            &dst,
        ]);
        c
    };
    for (src, dst) in [
        (builder_bin.to_string_lossy().to_string(), "/root/stormcos-builder"),
        (dc.ct_config.to_string_lossy().to_string(), "/root/builder.toml"),
    ] {
        let o = scp_to(src.clone(), dst).output().await?;
        anyhow::ensure!(
            o.status.success(),
            "staging {src} to CT {}: {}",
            ct.vmid,
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }

    // 2c. run the one-shot Rust build inside the container.
    let cmd = format!(
        "chmod +x /root/stormcos-builder && \
         /root/stormcos-builder build --config /root/builder.toml \
           --flavor '{flavor}' --release '{release_id}' --out /root/out \
           > /root/build.log 2>&1; rc=$?; \
         echo '---build.log tail---'; tail -n 200 /root/build.log; exit $rc"
    );
    let out = ssh(&["bash", "-lc", &cmd]).output().await?;
    append_log(log, &out.stdout);
    append_log(log, &out.stderr);
    anyhow::ensure!(
        out.status.success(),
        "in-container build of {} failed (CT {}): {}",
        dc.repo,
        ct.vmid,
        out.status
    );

    // 3. copy the manifest back and parse it.
    std::fs::create_dir_all(out_dir)?;
    let manifest = out_dir.join(format!("{release_id}.manifest.json"));
    let back = tokio::process::Command::new("scp")
        .args([
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            &format!("{host}:/root/out/manifest.json"),
            &manifest.to_string_lossy(),
        ])
        .output()
        .await?;
    anyhow::ensure!(
        back.status.success(),
        "retrieving manifest from CT {}: {}",
        ct.vmid,
        String::from_utf8_lossy(&back.stderr).trim()
    );
    let m: BuildManifest = serde_json::from_slice(&std::fs::read(&manifest)?)
        .map_err(|e| anyhow::anyhow!("parsing manifest from CT {}: {e}", ct.vmid))?;
    Ok((m.artifacts, m.targets))
}

fn append_log(log: &Path, bytes: &[u8]) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = f.write_all(bytes);
    }
}
