//! WebFetch, WebSearch backends with domain allow/block lists, and typed local Git integration.

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::{Duration, Instant},
};

/// Maximum response bytes read by `WebFetch` before truncation.
pub const MAX_WEB_BYTES: usize = 200_000;

/// Domain allow/block policy for `WebFetch` and `WebSearch`.
#[derive(Debug, Clone, Default)]
pub struct DomainPolicy {
    /// Allowed domains (if non-empty, only these domains or their subdomains are permitted).
    pub allowed: BTreeSet<String>,
    /// Explicitly blocked domains.
    pub blocked: BTreeSet<String>,
}

impl DomainPolicy {
    /// Extract the host portion from an HTTP or HTTPS URL.
    pub fn extract_host(url: &str) -> Option<&str> {
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))?;
        let host_port = rest.split(['/', '?', '#']).next()?;
        let host = host_port.split(':').next()?;
        if host.is_empty() {
            None
        } else {
            Some(host)
        }
    }

    fn matches_domain(host: &str, rule: &str) -> bool {
        let h = host.to_ascii_lowercase();
        let r = rule.to_ascii_lowercase();
        h == r || h.ends_with(&format!(".{r}"))
    }

    /// Check whether a URL is permitted by the domain allow/block policy.
    pub fn is_allowed_url(&self, url: &str) -> bool {
        let Ok(parsed) = reqwest::Url::parse(url) else {
            return false;
        };
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return false;
        }
        let Some(host) = parsed.host_str() else {
            return false;
        };
        if self.blocked.iter().any(|b| Self::matches_domain(host, b)) {
            return false;
        }
        if self.allowed.is_empty() {
            true
        } else {
            self.allowed.iter().any(|a| Self::matches_domain(host, a))
        }
    }
}

/// Convert raw HTML into clean readable Markdown while stripping `<script>` and `<style>` blocks.
pub fn html_to_markdown(html: &str) -> String {
    let mut cleaned = html.to_owned();
    for tag in ["script", "style"] {
        let open_pat = format!("<{tag}");
        let close_pat = format!("</{tag}>");
        while let Some(start) = cleaned.to_ascii_lowercase().find(&open_pat) {
            if let Some(rel_end) = cleaned[start..].to_ascii_lowercase().find(&close_pat) {
                cleaned.replace_range(start..start + rel_end + close_pat.len(), "");
            } else {
                cleaned.truncate(start);
                break;
            }
        }
    }

    let replacements = [
        ("<h1>", "\n# "),
        ("</h1>", "\n"),
        ("<h2>", "\n## "),
        ("</h2>", "\n"),
        ("<h3>", "\n### "),
        ("</h3>", "\n"),
        ("<li>", "\n- "),
        ("</li>", ""),
        ("<p>", "\n\n"),
        ("</p>", "\n"),
        ("<br>", "\n"),
        ("<br/>", "\n"),
        ("<br />", "\n"),
        ("<code>", "`"),
        ("</code>", "`"),
    ];
    for (from, to) in replacements {
        cleaned = cleaned.replace(from, to);
    }

    let mut out = String::with_capacity(cleaned.len());
    let mut in_tag = false;
    for ch in cleaned.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    let decoded = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    decoded
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Wrap web-originated content in explicit untrusted-data markers to defend against prompt injection.
pub fn wrap_untrusted_web(source_url: &str, body: &str) -> String {
    let safe_body = body.replace("</untrusted_web_content>", "[escaped-closing-tag]");
    let source_url = source_url
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace(['\r', '\n'], " ");
    format!(
        "<untrusted_web_content source=\"{source_url}\">\n{safe_body}\n</untrusted_web_content>"
    )
}

/// Cached `WebFetch` client with explicit redirect surfacing and a 15-minute TTL.
pub struct WebFetcher {
    policy: DomainPolicy,
    ttl: Duration,
    cache: Mutex<BTreeMap<String, (Instant, String)>>,
    client: reqwest::Client,
}

impl WebFetcher {
    /// Construct a `WebFetcher` with a domain policy and cache TTL (default 15 minutes).
    pub fn new(policy: DomainPolicy, ttl: Duration) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "cannot build WebFetcher HTTP client")?;
        Ok(Self {
            policy,
            ttl,
            cache: Mutex::new(BTreeMap::new()),
            client,
        })
    }

    /// Fetch a URL, convert HTML to Markdown, cache for 15 minutes, and wrap as untrusted data.
    /// Redirects are surfaced explicitly rather than followed silently across hosts.
    pub async fn fetch(&self, url: &str) -> Result<String, String> {
        if !self.policy.is_allowed_url(url) {
            return Err(format!("URL blocked by domain policy: {url}"));
        }
        if let Ok(guard) = self.cache.lock() {
            if let Some((ts, cached)) = guard.get(url) {
                if ts.elapsed() < self.ttl {
                    return Ok(cached.clone());
                }
            }
        }
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| "WebFetch request failed")?;
        if resp.status().is_redirection() {
            let loc = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("<unknown>");
            return Ok(wrap_untrusted_web(
                url,
                &format!("REDIRECT {}: {}", resp.status().as_u16(), loc),
            ));
        }
        if !resp.status().is_success() {
            return Err(format!("WebFetch HTTP {}", resp.status().as_u16()));
        }
        use futures_util::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "cannot read WebFetch body")?;
            let remaining = MAX_WEB_BYTES.saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            if bytes.len() >= MAX_WEB_BYTES {
                break;
            }
        }
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if text.len() > MAX_WEB_BYTES {
            let mut end = MAX_WEB_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
        let md = html_to_markdown(&text);
        let wrapped = wrap_untrusted_web(url, &md);
        if let Ok(mut guard) = self.cache.lock() {
            guard.insert(url.to_owned(), (Instant::now(), wrapped.clone()));
        }
        Ok(wrapped)
    }
}

/// One search hit returned by `WebSearch`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchHit {
    /// Page title.
    pub title: String,
    /// Target URL.
    pub url: String,
    /// Snippet text.
    pub snippet: String,
}

/// Pluggable `WebSearch` backend configuration (`SearXNG`, `Brave`, or `Tavily` JSON endpoints).
#[derive(Debug, Clone)]
pub struct WebSearchBackend {
    /// Endpoint URL accepting `?q=<query>&format=json`.
    pub endpoint: String,
    /// Optional API key header `(header_name, header_value)`.
    pub auth_header: Option<(String, String)>,
    /// Domain filter policy applied to returned results.
    pub policy: DomainPolicy,
}

impl WebSearchBackend {
    /// Execute a search query against the configured JSON search endpoint and wrap results.
    pub async fn search(&self, query: &str, limit: usize) -> Result<String, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "cannot build WebSearch client")?;
        let mut url =
            reqwest::Url::parse(&self.endpoint).map_err(|_| "invalid WebSearch endpoint")?;
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("format", "json");
        let mut req = client.get(url);
        if let Some((k, v)) = &self.auth_header {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|_| "WebSearch request failed")?;
        if !resp.status().is_success() {
            return Err(format!("WebSearch HTTP {}", resp.status().as_u16()));
        }
        const MAX_SEARCH_BYTES: usize = 2 * 1024 * 1024;
        if resp
            .content_length()
            .is_some_and(|size| size > MAX_SEARCH_BYTES as u64)
        {
            return Err("WebSearch response is too large".into());
        }
        use futures_util::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "cannot read WebSearch response")?;
            if bytes.len().saturating_add(chunk.len()) > MAX_SEARCH_BYTES {
                return Err("WebSearch response is too large".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let val: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "invalid WebSearch JSON payload")?;
        let items = val["results"]
            .as_array()
            .or_else(|| val["web"]["results"].as_array())
            .cloned()
            .unwrap_or_default();
        let mut lines = Vec::new();
        for item in items {
            let hit_url = item["url"].as_str().unwrap_or("");
            if !self.policy.is_allowed_url(hit_url) {
                continue;
            }
            let title = item["title"].as_str().unwrap_or("Untitled");
            let snippet = item["content"]
                .as_str()
                .or_else(|| item["description"].as_str())
                .unwrap_or("");
            lines.push(format!("- [{title}]({hit_url}): {snippet}"));
            if lines.len() >= limit {
                break;
            }
        }
        Ok(wrap_untrusted_web(&self.endpoint, &lines.join("\n")))
    }
}

/// Snapshot of local Git repository status for the TUI status bar and environment block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatusSummary {
    /// Current branch name or detached `HEAD` label.
    pub branch: String,
    /// Whether the working tree or index has uncommitted changes.
    pub dirty: bool,
    /// Commits ahead of upstream.
    pub ahead: u32,
    /// Commits behind upstream.
    pub behind: u32,
}

/// Thin typed wrapper around the system `git` executable.
#[derive(Debug, Clone)]
pub struct GitRepo {
    root: PathBuf,
}

impl GitRepo {
    /// Detect a Git repository containing `dir`.
    pub fn discover(dir: &Path) -> Option<Self> {
        let out = Command::new("git")
            .args(["-C", &dir.to_string_lossy(), "rev-parse", "--show-toplevel"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let root = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        Some(Self {
            root: PathBuf::from(root),
        })
    }

    /// Repository root path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Query branch, dirty flag, and ahead/behind counts via `git status --porcelain=v2 --branch`.
    pub fn status_summary(&self) -> Result<GitStatusSummary, String> {
        let out = Command::new("git")
            .current_dir(&self.root)
            .args(["status", "--porcelain=v2", "--branch"])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err("git status failed".into());
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut branch = "HEAD".to_owned();
        let mut ahead = 0u32;
        let mut behind = 0u32;
        let mut dirty = false;
        for line in text.lines() {
            if let Some(b) = line.strip_prefix("# branch.head ") {
                branch = b.trim().to_owned();
            } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
                for tok in ab.split_whitespace() {
                    if let Some(a) = tok.strip_prefix('+') {
                        ahead = a.parse().unwrap_or(0);
                    } else if let Some(bh) = tok.strip_prefix('-') {
                        behind = bh.parse().unwrap_or(0);
                    }
                }
            } else if !line.starts_with('#') && !line.trim().is_empty() {
                dirty = true;
            }
        }
        Ok(GitStatusSummary {
            branch,
            dirty,
            ahead,
            behind,
        })
    }

    /// Return unified diff output for `/diff` and `/review`.
    pub fn diff_stat_and_patch(&self, staged: bool) -> Result<String, String> {
        let mut args = vec!["diff", "--no-ext-diff"];
        if staged {
            args.push("--staged");
        }
        let out = Command::new("git")
            .current_dir(&self.root)
            .args(&args)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err("git diff failed".into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Draft a Conventional Commit message from the currently staged files for `/commit`.
    pub fn draft_commit_message(&self) -> Result<String, String> {
        let out = Command::new("git")
            .current_dir(&self.root)
            .args(["diff", "--staged", "--name-only"])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err("git diff --staged failed".into());
        }
        let files: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect();
        if files.is_empty() {
            return Err("no staged changes to commit".into());
        }
        let prefix = if files
            .iter()
            .all(|f| f.starts_with("docs/") || f.ends_with(".md"))
        {
            "docs"
        } else if files.iter().all(|f| f.starts_with("tests/")) {
            "test"
        } else {
            "feat"
        };
        let summary = if files.len() == 1 {
            format!("{prefix}: update {}", files[0])
        } else {
            format!("{prefix}: update {} files ({}, ...)", files.len(), files[0])
        };
        Ok(summary)
    }
}

/// Detect destructive Git commands (`reset --hard`, `push --force`, `clean -fd`, `checkout -- .`)
/// that must never run without explicit approval outside `Bypass` mode.
pub fn is_destructive_git(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    lower.contains("git reset --hard")
        || lower.contains("git push --force")
        || lower.contains("git push -f")
        || lower.contains("git clean -f")
        || lower.contains("git checkout -- .")
        || lower.contains("git branch -d")
}
