//! Poll watched GitHub repos for the HEAD sha of a branch.
//!
//! Uses the REST API with an optional token from $GITHUB_TOKEN / $GH_TOKEN
//! (public repos work unauthenticated but are rate-limited).

use crate::config::Component;

pub struct GitHub {
    client: reqwest::Client,
    token: Option<String>,
}

impl GitHub {
    pub fn new() -> Self {
        let token = std::env::var("GITHUB_TOKEN")
            .or_else(|_| std::env::var("GH_TOKEN"))
            .ok()
            .filter(|t| !t.is_empty());
        Self {
            client: reqwest::Client::builder()
                .user_agent("stormcos-builder")
                .build()
                .expect("reqwest client"),
            token,
        }
    }

    /// Current HEAD sha of `component.branch`.
    pub async fn head_sha(&self, c: &Component) -> anyhow::Result<String> {
        let url = format!(
            "https://api.github.com/repos/{}/commits/{}",
            c.repo, c.branch
        );
        let mut req = self.client.get(&url).header("Accept", "application/vnd.github+json");
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await?;
        anyhow::ensure!(
            resp.status().is_success(),
            "GitHub {} for {}: {}",
            resp.status(),
            c.repo,
            c.branch
        );
        let v: serde_json::Value = resp.json().await?;
        v.get("sha")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("no sha in commit response for {}", c.repo))
    }

    /// File an issue on `repo` (e.g. `glennswest/stormcos`) — used to report a
    /// build failure on the repo whose code was being built. Requires a token
    /// with `issues:write`; returns the new issue URL. Best-effort dedup is left
    /// to the caller (title/marker), matching component-builder's convention.
    pub async fn create_issue(
        &self,
        repo: &str,
        title: &str,
        body: &str,
    ) -> anyhow::Result<String> {
        let token = self
            .token
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("no GitHub token; cannot file issue on {repo}"))?;
        let url = format!("https://api.github.com/repos/{repo}/issues");
        let resp = self
            .client
            .post(&url)
            .header("Accept", "application/vnd.github+json")
            .bearer_auth(token)
            .json(&serde_json::json!({ "title": title, "body": body }))
            .send()
            .await?;
        anyhow::ensure!(
            resp.status().is_success(),
            "create issue on {repo}: HTTP {}",
            resp.status()
        );
        let v: serde_json::Value = resp.json().await?;
        Ok(v.get("html_url")
            .and_then(|u| u.as_str())
            .unwrap_or_default()
            .to_string())
    }

    /// Short one-line summaries of commits in `component.branch` since `since`
    /// (exclusive). Used for release notes. Best-effort; returns empty on error.
    pub async fn commits_since(&self, c: &Component, since: Option<&str>) -> Vec<String> {
        let url = format!(
            "https://api.github.com/repos/{}/commits?sha={}&per_page=30",
            c.repo, c.branch
        );
        let mut req = self.client.get(&url).header("Accept", "application/vnd.github+json");
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let Ok(resp) = req.send().await else {
            return vec![];
        };
        let Ok(v) = resp.json::<serde_json::Value>().await else {
            return vec![];
        };
        let Some(arr) = v.as_array() else { return vec![] };
        let mut out = Vec::new();
        for item in arr {
            let sha = item.get("sha").and_then(|s| s.as_str()).unwrap_or("");
            if Some(sha) == since {
                break; // reached the last release's sha
            }
            let msg = item
                .get("commit")
                .and_then(|c| c.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("");
            out.push(format!("{} {}", &sha.chars().take(7).collect::<String>(), msg));
        }
        out
    }

    /// The latest release's tag + the API download URL of the first asset whose
    /// name matches `pattern` (a glob with `*`). Returns None if no release or no
    /// matching asset. Used to harvest a component's built asset per build.
    pub async fn latest_release_asset(
        &self,
        repo: &str,
        pattern: &str,
    ) -> anyhow::Result<Option<(String, String)>> {
        let url = format!("https://api.github.com/repos/{repo}/releases/latest");
        let mut req = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.github+json");
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let v: serde_json::Value = resp.json().await?;
        let tag = v
            .get("tag_name")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        for a in v.get("assets").and_then(|a| a.as_array()).into_iter().flatten() {
            let name = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if glob_match(pattern, name) {
                // The asset API url (not browser_download_url) works for private
                // repos with Accept: application/octet-stream + token.
                let dl = a.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
                return Ok(Some((tag, dl)));
            }
        }
        Ok(None)
    }

    /// Download a release asset (by its API `url`) to `dest`.
    pub async fn download_asset(
        &self,
        asset_url: &str,
        dest: &std::path::Path,
    ) -> anyhow::Result<()> {
        let mut req = self
            .client
            .get(asset_url)
            .header("Accept", "application/octet-stream");
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await?;
        anyhow::ensure!(
            resp.status().is_success(),
            "download {asset_url}: HTTP {}",
            resp.status()
        );
        let bytes = resp.bytes().await?;
        std::fs::write(dest, &bytes)?;
        Ok(())
    }
}

/// Minimal glob (`*` = any run) matcher for release asset names — avoids a regex
/// dependency. `prefix*middle*suffix`.
fn glob_match(pattern: &str, s: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == s;
    }
    if !s.starts_with(parts[0]) {
        return false;
    }
    let mut pos = parts[0].len();
    for (i, part) in parts.iter().enumerate().skip(1) {
        if i == parts.len() - 1 {
            return s[pos..].ends_with(part);
        }
        if part.is_empty() {
            continue;
        }
        match s[pos..].find(part) {
            Some(idx) => pos += idx + part.len(),
            None => return false,
        }
    }
    true
}
