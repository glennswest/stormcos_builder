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
}
