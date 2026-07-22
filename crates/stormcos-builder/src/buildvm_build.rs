//! Run the image build in an ephemeral Proxmox VM cloned from the golden
//! template. The build is the builder's job in Rust: the VM runs the SAME
//! builder binary (`stormcos-builder build ...`), not a shell script. A VM has
//! its own kernel, so ublk/stormblock slab assembly needs no privilege.
//!
//! `buildvm-driver` owns the lifecycle — clone the template, stage the builder
//! binary + config, run the build, fetch the manifest, and ALWAYS destroy the
//! clone. Here we just describe the job and parse the result.
//!
//! A failure surfaces as `Err`; the caller files an issue on the repo being
//! built (config::BuildVm::repo).

use std::path::Path;

use buildvm_driver::{BuildJob, Cloner};

use crate::config::{BuildVm as BuildVmCfg, Config};
use crate::jobs::BuildManifest;
use crate::model::{Artifact, NetworkTarget};

/// Build `flavor`/`release_id` in a fresh clone and return the artifacts +
/// network targets it produced.
pub async fn build(
    cfg: &Config,
    bc: &BuildVmCfg,
    flavor: &str,
    release_id: &str,
    out_dir: &Path,
    log: &Path,
) -> anyhow::Result<(Vec<Artifact>, Vec<NetworkTarget>)> {
    let _ = cfg;
    let token = bc.token();
    anyhow::ensure!(
        !token.is_empty(),
        "buildvm: no API token (set [buildvm].token or $DEVCT_TOKEN/$PROXMOX_API_TOKEN)"
    );
    let cloner = Cloner::new(&bc.api, &bc.node, &token)?;

    let builder_bin = match &bc.builder_bin {
        Some(p) => p.clone(),
        None => std::env::current_exe()?,
    };
    std::fs::create_dir_all(out_dir)?;
    let manifest_local = out_dir.join(format!("{release_id}.manifest.json"));
    let project = bc.repo.rsplit('/').next().unwrap_or(&bc.repo);

    // The build is the builder itself: stage the binary + config, run the
    // one-shot `build` subcommand (Rust pipeline), fetch the manifest it writes.
    let command = format!(
        "chmod +x /root/stormcos-builder && \
         /root/stormcos-builder build --config /root/builder.toml \
           --flavor '{flavor}' --release '{release_id}' --out /root/out \
           > /root/build.log 2>&1; rc=$?; \
         echo '---build.log tail---'; tail -n 200 /root/build.log; exit $rc"
    );
    let job = BuildJob {
        template: bc.template,
        name: format!("build-{project}-{release_id}"),
        cores: bc.cores,
        memory_mb: bc.memory_mb,
        ssh_user: bc.ssh_user.clone(),
        ssh_key: bc.ssh_key.clone(),
        stage: vec![
            (builder_bin, "/root/stormcos-builder".to_string()),
            (bc.vm_config.clone(), "/root/builder.toml".to_string()),
        ],
        command,
        fetch: vec![("/root/out/manifest.json".to_string(), manifest_local.clone())],
    };

    // Driver clones -> builds -> ALWAYS destroys.
    let out = cloner.run_build(&job).await?;
    append_log(log, &out.stdout);
    append_log(log, &out.stderr);

    let m: BuildManifest = serde_json::from_slice(&std::fs::read(&manifest_local)?)
        .map_err(|e| anyhow::anyhow!("parsing manifest from build VM: {e}"))?;
    Ok((m.artifacts, m.targets))
}

fn append_log(log: &Path, bytes: &[u8]) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = f.write_all(bytes);
    }
}
