//! REST API.

use std::sync::Arc;

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use tokio_util::io::ReaderStream;

use crate::App;
use crate::jobs;
use crate::model::{BootMethod, Format};

type Err = (StatusCode, Json<serde_json::Value>);
fn err(code: StatusCode, msg: impl Into<String>) -> Err {
    (code, Json(json!({ "error": msg.into() })))
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/components", get(components))
        .route("/api/v1/flavors", get(flavors))
        .route("/api/v1/flavors/:flavor/build", post(build_flavor))
        .route("/api/v1/builds", get(builds))
        .route("/api/v1/builds/:id", get(build_get))
        .route("/api/v1/releases", get(releases))
        .route("/api/v1/releases/:id", get(release_get))
        .route("/api/v1/releases/:id/download/:format", get(download))
        .route("/api/v1/clusters", get(clusters).post(create_cluster))
        .route("/api/v1/clusters/:name", get(cluster_get).delete(delete_cluster))
        .route("/api/v1/clusters/:name/rebuild", post(rebuild_cluster))
        .with_state(app)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "service": "stormcos-builder" }))
}

async fn components(State(app): State<Arc<App>>) -> Json<serde_json::Value> {
    let c: Vec<_> = app
        .cfg
        .components
        .iter()
        .map(|c| json!({ "name": c.name, "repo": c.repo, "branch": c.branch }))
        .collect();
    Json(json!({ "components": c }))
}

async fn flavors(State(app): State<Arc<App>>) -> Json<serde_json::Value> {
    let f: Vec<_> = app
        .cfg
        .flavors
        .iter()
        .map(|f| {
            json!({
                "name": f.name,
                "description": f.description,
                "extends": f.extends,
                "assets": app.cfg.flavor_asset_names(&f.name),
            })
        })
        .collect();
    Json(json!({ "flavors": f }))
}

async fn build_flavor(
    State(app): State<Arc<App>>,
    Path(flavor): Path<String>,
) -> Result<Json<serde_json::Value>, Err> {
    if app.cfg.flavor(&flavor).is_none() {
        return Err(err(StatusCode::NOT_FOUND, format!("unknown flavor {flavor}")));
    }
    let app2 = app.clone();
    let f = flavor.clone();
    // The build id is derived inside jobs::build; kick it off and return.
    tokio::spawn(async move {
        jobs::build(app2, f, "manual (API)".into()).await;
    });
    Ok(Json(json!({ "flavor": flavor, "status": "build queued" })))
}

async fn builds(State(app): State<Arc<App>>) -> Json<serde_json::Value> {
    Json(json!({ "builds": app.read(|s| s.builds.clone()).await }))
}

async fn build_get(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, Err> {
    app.read(|s| s.builds.iter().find(|b| b.id == id).cloned())
        .await
        .map(|b| Json(json!(b)))
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such build"))
}

#[derive(Deserialize)]
struct RelQuery {
    flavor: Option<String>,
}

async fn releases(
    State(app): State<Arc<App>>,
    Query(q): Query<RelQuery>,
) -> Json<serde_json::Value> {
    let list = app
        .read(|s| {
            s.releases
                .iter()
                .rev()
                .filter(|r| q.flavor.as_ref().is_none_or(|f| &r.flavor == f))
                .cloned()
                .collect::<Vec<_>>()
        })
        .await;
    Json(json!({ "releases": list }))
}

async fn release_get(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, Err> {
    app.read(|s| s.release(&id).cloned())
        .await
        .map(|r| Json(json!(r)))
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such release"))
}

async fn download(
    State(app): State<Arc<App>>,
    Path((id, format)): Path<(String, String)>,
) -> Result<Response, Err> {
    let fmt = Format::parse(&format)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "format must be img|qcow2|iso"))?;
    let art = app
        .read(|s| s.release(&id).and_then(|r| r.artifact(fmt).cloned()))
        .await
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such release/format"))?;
    let file = tokio::fs::File::open(&art.path)
        .await
        .map_err(|e| err(StatusCode::NOT_FOUND, format!("image file gone: {e}")))?;
    let body = Body::from_stream(ReaderStream::new(file));
    let fname = format!("{id}.{}", fmt.ext());
    Ok((
        [
            (header::CONTENT_TYPE, fmt.content_type().to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{fname}\""),
            ),
            (header::CONTENT_LENGTH, art.bytes.to_string()),
        ],
        body,
    )
        .into_response())
}

async fn clusters(State(app): State<Arc<App>>) -> Json<serde_json::Value> {
    Json(json!({ "clusters": app.read(|s| s.clusters.clone()).await }))
}

#[derive(Deserialize)]
struct CreateCluster {
    name: String,
    dns_name: Option<String>,
    flavor: Option<String>,
    release_id: Option<String>,
    boot_method: Option<String>,
}

async fn create_cluster(
    State(app): State<Arc<App>>,
    Json(req): Json<CreateCluster>,
) -> Result<Json<serde_json::Value>, Err> {
    if !dns_safe(&req.name) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "name must be DNS-safe [a-z0-9-]",
        ));
    }
    let boot = BootMethod::parse(req.boot_method.as_deref().unwrap_or("local-disk"))
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "boot_method: local-disk|iscsi|nvme-tcp"))?;

    // Pick release: explicit id, else latest release of the requested flavor,
    // else the latest release overall.
    let release_id = app
        .read(|s| {
            if let Some(id) = &req.release_id {
                return s.release(id).map(|r| r.id.clone());
            }
            if let Some(fl) = &req.flavor {
                return s
                    .releases
                    .iter()
                    .rev()
                    .find(|r| &r.flavor == fl)
                    .map(|r| r.id.clone());
            }
            s.latest_release().map(|r| r.id.clone())
        })
        .await
        .ok_or_else(|| err(StatusCode::CONFLICT, "no matching release built yet"))?;

    let dns = req
        .dns_name
        .unwrap_or_else(|| format!("{}.g8.lo", req.name));
    let (app2, name, rel) = (app.clone(), req.name.clone(), release_id.clone());
    tokio::spawn(async move {
        jobs::provision(app2, name, dns, rel, boot).await;
    });
    Ok(Json(json!({
        "name": req.name,
        "release_id": release_id,
        "boot_method": req.boot_method.unwrap_or_else(|| "local-disk".into()),
        "status": "provisioning"
    })))
}

async fn cluster_get(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, Err> {
    app.read(|s| s.cluster(&name).cloned())
        .await
        .map(|c| Json(json!(c)))
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such cluster"))
}

async fn rebuild_cluster(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, Err> {
    if app.read(|s| s.cluster(&name).is_none()).await {
        return Err(err(StatusCode::NOT_FOUND, "no such cluster"));
    }
    let (app2, n) = (app.clone(), name.clone());
    tokio::spawn(async move {
        jobs::rebuild(app2, n).await;
    });
    Ok(Json(json!({ "name": name, "status": "rebuilding" })))
}

async fn delete_cluster(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, Err> {
    if app.read(|s| s.cluster(&name).is_none()).await {
        return Err(err(StatusCode::NOT_FOUND, "no such cluster"));
    }
    let (app2, n) = (app.clone(), name.clone());
    tokio::spawn(async move {
        jobs::delete(app2, n).await;
    });
    Ok(Json(json!({ "name": name, "status": "deleting" })))
}

fn dns_safe(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}
