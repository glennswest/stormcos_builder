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

    // Prefer the in-process Rust pipeline; fall back to the build script.
    let result: anyhow::Result<(Vec<Artifact>, Vec<NetworkTarget>)> =
        if app.cfg.pipeline.is_some() {
            crate::pipeline::build(&app.cfg, &flavor, &id, &images, &log).await
        } else {
            // Args: flavor, release-id, out-dir, manifest-path, comma-joined assets.
            let args = vec![
                flavor.clone(),
                id.clone(),
                images.to_string_lossy().to_string(),
                manifest.to_string_lossy().to_string(),
                assets.join(","),
            ];
            run(&app.cfg.scripts.build_image, &args, &log).await.map(|()| {
                std::fs::read(&manifest)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<BuildManifest>(&b).ok())
                    .map(|m| (m.artifacts, m.targets))
                    .unwrap_or_default()
            })
        };

    app.mutate(|s| {
        let ok = result.is_ok();
        if let Some(b) = s.build_mut(&id) {
            b.status = if ok { Status::Success } else { Status::Failed };
            b.finished = Some(now());
            if ok {
                b.release_id = Some(id.clone());
            }
        }
        if let Ok((artifacts, targets)) = &result {
            let (artifacts, targets) = (artifacts.clone(), targets.clone());
            s.releases.push(Release {
                id: id.clone(),
                flavor: flavor.clone(),
                created: now(),
                components: shas.clone(),
                notes: notes.clone(),
                artifacts,
                targets,
                tombstoned: false,
                qa: None,
            });
            for (name, sha) in &shas {
                s.last_seen.insert(name.clone(), sha.clone());
            }
        }
    })
    .await;

    // QA pass over the fresh image (image-scope; cluster-scope runs post-provision).
    if result.is_ok() {
        run_qa(app.clone(), &id).await;
        // Reclaim disk automatically: intermediates are dead the moment the
        // image exists (for EVERY finished build, not just this one), and we
        // keep only the last N releases.
        sweep_intermediates(app.clone()).await;
        prune_old_releases(app.clone(), &flavor).await;
    }
    id
}

/// Intermediate name prefixes in images/. Everything matching these is scratch
/// produced during a build; only `<release>.img/.qcow2/.iso` are keepers.
const INTERMEDIATE_PREFIXES: [&str; 6] = [
    "edition-",
    "vol-",
    "irfs-",
    "base-initramfs-",
    "initramfs-meta-",
    "initramfs-",
];

/// Sweep intermediates left by ANY finished build, not just the one that just
/// ran. Intermediates of a *kept* release are still scratch, and builds made
/// before pruning existed leave theirs behind forever otherwise.
///
/// Skips anything belonging to a build that is still running, so a concurrent
/// build never has its working dirs pulled out from under it.
async fn sweep_intermediates(app: Arc<App>) {
    let running: Vec<String> = app
        .read(|s| {
            s.builds
                .iter()
                .filter(|b| matches!(b.status, Status::Running))
                .map(|b| b.id.clone())
                .collect()
        })
        .await;
    // Safety net: never delete a file some release advertises for download,
    // whatever its name looks like (a flavor named "edition"/"vol" would
    // otherwise collide with an intermediate prefix).
    let keep: Vec<std::path::PathBuf> = app
        .read(|s| {
            s.releases
                .iter()
                .flat_map(|r| r.artifacts.iter().map(|a| a.path.clone()))
                .collect()
        })
        .await;

    let images = app.cfg.images_dir();
    let Ok(entries) = std::fs::read_dir(&images) else {
        return;
    };
    let mut freed = 0u64;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(pfx) = INTERMEDIATE_PREFIXES
            .iter()
            .find(|p| name.starts_with(**p))
        else {
            continue;
        };
        // `<prefix><release-id>` (dirs) or `<prefix><release-id>.img` (files).
        let id = name[pfx.len()..].trim_end_matches(".img");
        if running.iter().any(|r| r == id) {
            continue;
        }
        let p = e.path();
        if keep.iter().any(|k| *k == p) {
            continue; // a real, downloadable release artifact
        }
        freed += dir_size(&p);
        let r = if p.is_dir() {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
        if let Err(err) = r {
            tracing::warn!("sweep {}: {err}", p.display());
        }
    }
    if freed > 0 {
        tracing::info!(
            "swept build intermediates, freed {:.1} GB",
            freed as f64 / (1024.0 * 1024.0 * 1024.0)
        );
    }
}

/// Apparent size of a file or directory tree (best-effort, for logging).
fn dir_size(p: &std::path::Path) -> u64 {
    let Ok(md) = std::fs::metadata(p) else {
        return 0;
    };
    if md.is_file() {
        return md.len();
    }
    let Ok(entries) = std::fs::read_dir(p) else {
        return 0;
    };
    entries.flatten().map(|e| dir_size(&e.path())).sum()
}

/// Delete a finished build's intermediate working dirs/files. These are pure
/// scratch — the edition staging tree, the slab artifact dir, and the initramfs
/// assembly steps — and together they dwarf the artifacts we actually keep.
fn prune_intermediates(app: &Arc<App>, release_id: &str) {
    let images = app.cfg.images_dir();
    let dirs = [
        format!("edition-{release_id}"),
        format!("vol-{release_id}"),
        format!("irfs-{release_id}"),
    ];
    let files = [
        format!("base-initramfs-{release_id}.img"),
        format!("initramfs-meta-{release_id}.img"),
        format!("initramfs-{release_id}.img"),
    ];
    for d in &dirs {
        let p = images.join(d);
        if p.is_dir() {
            if let Err(e) = std::fs::remove_dir_all(&p) {
                tracing::warn!("prune {}: {e}", p.display());
            }
        }
    }
    for f in &files {
        let p = images.join(f);
        if p.is_file() {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Keep only the newest `keep_releases` releases for `flavor`; delete the rest
/// (artifacts, build log, and state record). Per-flavor so a rarely-built
/// flavor isn't evicted by a busy one.
async fn prune_old_releases(app: Arc<App>, flavor: &str) {
    let keep = app.cfg.keep_releases;
    if keep == 0 {
        return; // pruning disabled
    }

    // Newest-first within this flavor; `created` is RFC3339 so it sorts lexically.
    let doomed: Vec<String> = app
        .read(|s| {
            let mut mine: Vec<&Release> =
                s.releases.iter().filter(|r| r.flavor == flavor).collect();
            mine.sort_by(|a, b| b.created.cmp(&a.created));
            mine.into_iter()
                .skip(keep)
                .map(|r| r.id.clone())
                .collect()
        })
        .await;
    if doomed.is_empty() {
        return;
    }

    // Remove files first, then the records — a record without its artifacts
    // would advertise a download that 404s.
    let paths: Vec<std::path::PathBuf> = app
        .read(|s| {
            s.releases
                .iter()
                .filter(|r| doomed.contains(&r.id))
                .flat_map(|r| r.artifacts.iter().map(|a| a.path.clone()))
                .collect()
        })
        .await;
    let mut freed = 0u64;
    for p in paths {
        if let Ok(md) = std::fs::metadata(&p) {
            freed += md.len();
        }
        if let Err(e) = std::fs::remove_file(&p) {
            tracing::warn!("prune artifact {}: {e}", p.display());
        }
    }
    for id in &doomed {
        let log = app.cfg.logs_dir().join(format!("build-{id}.log"));
        let _ = std::fs::remove_file(&log);
        prune_intermediates(&app, id);
    }

    app.mutate(|s| {
        s.releases.retain(|r| !doomed.contains(&r.id));
        s.builds.retain(|b| !doomed.contains(&b.id));
    })
    .await;

    tracing::info!(
        "pruned {} old {flavor} release(s), freed {:.1} GB (keep_releases={keep})",
        doomed.len(),
        freed as f64 / (1024.0 * 1024.0 * 1024.0)
    );
}

#[derive(serde::Deserialize)]
struct QaReport {
    total: usize,
    passed: usize,
    failed: usize,
    blocking_failures: usize,
}

/// Run qa-runner against a release's image; tombstone on blocking failure.
async fn run_qa(app: Arc<App>, release_id: &str) {
    let Some(qa) = &app.cfg.qa else { return };
    // Locate the raw image artifact for image-scope tests.
    let (image, flavor) = app
        .read(|s| {
            s.release(release_id).map(|r| {
                (
                    r.artifacts
                        .iter()
                        .find(|a| a.format == crate::model::Format::Img)
                        .map(|a| a.path.clone()),
                    r.flavor.clone(),
                )
            })
        })
        .await
        .unwrap_or((None, String::new()));
    let report = app
        .cfg
        .logs_dir()
        .join(format!("qa-{release_id}.json"));
    let mut args = vec![
        "--tests-dir".to_string(),
        qa.tests_dir.to_string_lossy().to_string(),
        "--release".to_string(),
        release_id.to_string(),
        "--flavor".to_string(),
        flavor,
        "--report".to_string(),
        report.to_string_lossy().to_string(),
    ];
    if let Some(img) = image {
        args.push("--image".into());
        args.push(img.to_string_lossy().to_string());
    }
    if qa.file_issues {
        args.push("--file-issues".into());
    }
    let _ = Command::new(&qa.runner).args(&args).status().await;

    let parsed: Option<QaReport> = std::fs::read(&report)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    app.mutate(|s| {
        if let Some(r) = s.releases.iter_mut().find(|r| r.id == release_id)
            && let Some(p) = &parsed
        {
            r.qa = Some(crate::model::QaResult {
                total: p.total,
                passed: p.passed,
                failed: p.failed,
                blocking_failures: p.blocking_failures,
                report: Some(report.clone()),
            });
            r.tombstoned = p.blocking_failures > 0;
        }
    })
    .await;
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
                // {e:#} prints the whole anyhow cause chain — reqwest's own
                // Display is just "error sending request for url (...)", which
                // hides whether it was DNS, TLS, connect or timeout.
                Err(e) => tracing::warn!("poll {}: {e:#}", c.repo),
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
