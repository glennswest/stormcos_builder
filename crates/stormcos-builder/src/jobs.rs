//! Job engine: run the external scripts that build boot images and provision
//! clusters, capture their logs, and update persisted state.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use chrono::Utc;
use tokio::process::Command;
use uuid::Uuid;

use crate::App;
use crate::model::{
    Artifact, BootMethod, Build, Cluster, ClusterPhase, NetworkTarget, Release, Status,
};

fn now() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
fn short_id() -> String {
    Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// Run a script with args, teeing stdout+stderr into `log`. Returns Ok on
/// exit 0.
async fn run(script: &str, args: &[String], log: &Path) -> anyhow::Result<()> {
    if let Some(d) = log.parent() {
        std::fs::create_dir_all(d)?;
    }
    let out = std::fs::File::create(log)?;
    let err = out.try_clone()?;
    let status = Command::new(script)
        .args(args)
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .status()
        .await
        .map_err(|e| anyhow::anyhow!("spawn {script}: {e}"))?;
    anyhow::ensure!(status.success(), "{script} exited {status}");
    Ok(())
}

/// What a build script writes next to its images so the service can register
/// the release's artifacts + network targets.
#[derive(serde::Deserialize)]
struct BuildManifest {
    #[serde(default)]
    artifacts: Vec<Artifact>,
    #[serde(default)]
    targets: Vec<NetworkTarget>,
}

/// Trigger a boot-image build for `flavor`. Records a Build, runs the build
/// script (passed the flavor + its asset list), and on success registers a
/// Release (component shas + generated notes + artifacts + network targets).
pub async fn build(app: Arc<App>, flavor: String, reason: String) -> String {
    let id = format!("{}-{}-{}", flavor, Utc::now().format("%Y-%m-%d"), short_id());
    let log = app.cfg.logs_dir().join(format!("build-{id}.log"));
    let build = Build {
        id: id.clone(),
        status: Status::Running,
        reason: reason.clone(),
        started: now(),
        finished: None,
        log: log.clone(),
        release_id: None,
    };
    app.mutate(|s| s.builds.push(build)).await;

    // Component shas + release notes for this flavor's assets (diff vs previous).
    let (shas, notes) = app.compute_release_notes(&flavor).await;
    let assets: Vec<String> = shas.keys().cloned().collect();

    let images = app.cfg.images_dir();
    let _ = std::fs::create_dir_all(&images);
    let manifest = images.join(format!("{id}.manifest.json"));
    // Args: flavor, release-id, out-dir, manifest-path, comma-joined assets.
    let args = vec![
        flavor.clone(),
        id.clone(),
        images.to_string_lossy().to_string(),
        manifest.to_string_lossy().to_string(),
        assets.join(","),
    ];

    let result = run(&app.cfg.scripts.build_image, &args, &log).await;

    app.mutate(|s| {
        let ok = result.is_ok();
        if let Some(b) = s.build_mut(&id) {
            b.status = if ok { Status::Success } else { Status::Failed };
            b.finished = Some(now());
            if ok {
                b.release_id = Some(id.clone());
            }
        }
        if ok {
            let (artifacts, targets) = std::fs::read(&manifest)
                .ok()
                .and_then(|b| serde_json::from_slice::<BuildManifest>(&b).ok())
                .map(|m| (m.artifacts, m.targets))
                .unwrap_or_default();
            s.releases.push(Release {
                id: id.clone(),
                flavor: flavor.clone(),
                created: now(),
                components: shas.clone(),
                notes: notes.clone(),
                artifacts,
                targets,
            });
            for (name, sha) in &shas {
                s.last_seen.insert(name.clone(), sha.clone());
            }
        }
    })
    .await;
    id
}

/// Provision a single-node cluster from a release, addressed by name/DNS.
pub async fn provision(
    app: Arc<App>,
    name: String,
    dns_name: String,
    release_id: String,
    boot: BootMethod,
) {
    let log = app.cfg.logs_dir().join(format!("cluster-{name}.log"));
    let image = app.release_image_arg(&release_id, boot);
    let cluster = Cluster {
        name: name.clone(),
        dns_name: dns_name.clone(),
        phase: ClusterPhase::Provisioning,
        release_id,
        boot_method: boot,
        ip: None,
        created: now(),
        log: log.clone(),
        message: "provisioning".into(),
    };
    app.mutate(|s| {
        s.clusters.retain(|c| c.name != name);
        s.clusters.push(cluster);
    })
    .await;

    let args = vec![name.clone(), dns_name, boot_arg(boot).into(), image];
    let result = run(&app.cfg.scripts.provision_cluster, &args, &log).await;
    let ip = ip_from_log(&log);
    app.mutate(|s| {
        if let Some(c) = s.cluster_mut(&name) {
            match &result {
                Ok(()) => {
                    c.phase = ClusterPhase::Ready;
                    c.ip = ip;
                    c.message = "ready".into();
                }
                Err(e) => {
                    c.phase = ClusterPhase::Failed;
                    c.message = e.to_string();
                }
            }
        }
    })
    .await;
}

/// Wipe + rebuild an existing test machine in place (fast reprovision).
pub async fn rebuild(app: Arc<App>, name: String) {
    let Some((rel, boot, log)) = app
        .read(|s| {
            s.cluster(&name)
                .map(|c| (c.release_id.clone(), c.boot_method, c.log.clone()))
        })
        .await
    else {
        return;
    };
    app.mutate(|s| {
        if let Some(c) = s.cluster_mut(&name) {
            c.phase = ClusterPhase::Rebuilding;
            c.message = "wiping + rebuilding".into();
        }
    })
    .await;
    let image = app.release_image_arg(&rel, boot);
    let args = vec![name.clone(), image];
    let result = run(&app.cfg.scripts.rebuild_machine, &args, &log).await;
    app.mutate(|s| {
        if let Some(c) = s.cluster_mut(&name) {
            match &result {
                Ok(()) => {
                    c.phase = ClusterPhase::Ready;
                    c.message = "rebuilt".into();
                }
                Err(e) => {
                    c.phase = ClusterPhase::Failed;
                    c.message = e.to_string();
                }
            }
        }
    })
    .await;
}

/// Tear a cluster down and forget it.
pub async fn delete(app: Arc<App>, name: String) {
    let log = app.cfg.logs_dir().join(format!("cluster-{name}.log"));
    app.mutate(|s| {
        if let Some(c) = s.cluster_mut(&name) {
            c.phase = ClusterPhase::Deleting;
        }
    })
    .await;
    let _ = run(
        &app.cfg.scripts.delete_cluster,
        std::slice::from_ref(&name),
        &log,
    )
    .await;
    app.mutate(|s| s.clusters.retain(|c| c.name != name)).await;
}

/// Background watcher: poll components; on change, optionally auto-build.
pub async fn watcher(app: Arc<App>) {
    let interval = std::time::Duration::from_secs(app.cfg.poll_interval_secs.max(30));
    loop {
        tokio::time::sleep(interval).await;
        let mut changed = Vec::new();
        for c in &app.cfg.components {
            match app.gh.head_sha(c).await {
                Ok(sha) => {
                    let prev = app.read(|s| s.last_seen.get(&c.name).cloned()).await;
                    if prev.as_deref() != Some(sha.as_str()) {
                        changed.push(c.name.clone());
                        app.mutate(|s| {
                            s.last_seen.insert(c.name.clone(), sha.clone());
                        })
                        .await;
                    }
                }
                Err(e) => tracing::warn!("poll {}: {e}", c.repo),
            }
        }
        if !changed.is_empty() && app.cfg.auto_build {
            // Rebuild every flavor whose resolved (layered) assets include a
            // changed component.
            for f in &app.cfg.flavors {
                let resolved = app.cfg.flavor_asset_names(&f.name);
                let hit: Vec<&String> =
                    changed.iter().filter(|c| resolved.contains(c)).collect();
                if hit.is_empty() {
                    continue;
                }
                let reason = format!(
                    "assets changed: {}",
                    hit.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                );
                tracing::info!("flavor {} — {reason}, building", f.name);
                let app2 = app.clone();
                let fname = f.name.clone();
                tokio::spawn(async move {
                    build(app2, fname, reason).await;
                });
            }
        }
    }
}

fn boot_arg(b: BootMethod) -> &'static str {
    match b {
        BootMethod::LocalDisk => "local-disk",
        BootMethod::Iscsi => "iscsi",
        BootMethod::NvmeTcp => "nvme-tcp",
    }
}

/// Scripts print `IP=<addr>` on a line when they learn the node's address.
fn ip_from_log(log: &Path) -> Option<String> {
    let text = std::fs::read_to_string(log).ok()?;
    text.lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("IP=").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
}
