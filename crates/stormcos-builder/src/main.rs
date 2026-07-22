//! stormcos-builder — watches the stormcos component repos, builds boot images
//! per flavor (layered base -> ... -> release), serves them for download
//! (img/qcow2/iso) and as network boot targets (iSCSI/NVMe-oF/TCP), and
//! provisions/rebuilds single-node clusters on demand by name — over a web UI
//! and a REST API. Not a registry: it delivers stormcos boot artifacts.

mod api;
mod config;
mod github;
mod jobs;
mod model;
mod pipeline;
mod devct_build;
#[allow(dead_code)]
mod reconcile;
mod web;

use std::sync::Arc;

use tokio::sync::RwLock;

use config::Config;
use github::GitHub;
use model::{BootMethod, ComponentShas, State};

pub struct App {
    pub cfg: Config,
    pub gh: GitHub,
    state: RwLock<State>,
}

impl App {
    pub async fn read<T>(&self, f: impl FnOnce(&State) -> T) -> T {
        f(&*self.state.read().await)
    }

    /// Mutate state under lock, then persist to the state file.
    pub async fn mutate(&self, f: impl FnOnce(&mut State)) {
        let mut g = self.state.write().await;
        f(&mut g);
        if let Err(e) = g.save(&self.cfg.state_file()) {
            tracing::error!("persist state: {e}");
        }
    }

    /// Current head shas for a flavor's assets + generated release notes
    /// (per-asset commits since the last time we saw that asset).
    pub async fn compute_release_notes(&self, flavor: &str) -> (ComponentShas, String) {
        let mut shas = ComponentShas::new();
        let mut notes = format!("# {flavor} boot image\n\n");
        for c in self.cfg.flavor_components(flavor) {
            let sha = match self.gh.head_sha(c).await {
                Ok(s) => s,
                Err(e) => {
                    notes.push_str(&format!("- **{}**: (poll failed: {e})\n", c.name));
                    continue;
                }
            };
            let since = self.read(|s| s.last_seen.get(&c.name).cloned()).await;
            let commits = self.gh.commits_since(c, since.as_deref()).await;
            notes.push_str(&format!(
                "## {} ({}) — {}\n",
                c.name,
                &sha.chars().take(7).collect::<String>(),
                c.repo
            ));
            if commits.is_empty() {
                notes.push_str("- (no new commits since last release)\n");
            } else {
                for line in &commits {
                    notes.push_str(&format!("- {line}\n"));
                }
            }
            notes.push('\n');
            shas.insert(c.name.clone(), sha);
        }
        (shas, notes)
    }

    /// The image/target argument passed to provision/rebuild scripts for a
    /// release + boot method: a local disk-image path, or a target URI.
    pub fn release_image_arg(&self, release_id: &str, boot: BootMethod) -> String {
        // Read is sync-free here: we look at the (already-loaded) state via a
        // blocking read is avoided; instead callers pass release_id and we
        // resolve from a fresh read is not possible in a sync fn — so resolve
        // by convention from the images dir + known extension.
        match boot {
            BootMethod::LocalDisk => self
                .cfg
                .images_dir()
                .join(format!("{release_id}.img"))
                .to_string_lossy()
                .to_string(),
            BootMethod::Iscsi => format!("iscsi:{release_id}"),
            BootMethod::NvmeTcp => format!("nvme-tcp:{release_id}"),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let cfg_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config/stormcos-builder.toml".to_string());
    let cfg = Config::load(std::path::Path::new(&cfg_path))?;
    std::fs::create_dir_all(cfg.images_dir())?;
    std::fs::create_dir_all(cfg.logs_dir())?;
    let state = State::load(&cfg.state_file());

    let app = Arc::new(App {
        gh: GitHub::new(),
        state: RwLock::new(state),
        cfg,
    });

    tokio::spawn(jobs::watcher(app.clone()));

    let router = api::router(app.clone()).merge(web::router());
    let listener = tokio::net::TcpListener::bind(&app.cfg.listen).await?;
    tracing::info!(
        "stormcos-builder listening on {} — {} flavors, {} components",
        app.cfg.listen,
        app.cfg.flavors.len(),
        app.cfg.components.len()
    );
    axum::serve(listener, router).await?;
    Ok(())
}
