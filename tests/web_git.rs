//! Tests for Milestone M14 WebFetch, WebSearch backends, untrusted wrapping, and GitRepo wrapper.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use gitcode::web_git;

use std::{collections::BTreeSet, process::Command, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use web_git::{
    html_to_markdown, is_destructive_git, wrap_untrusted_web, DomainPolicy, GitRepo, WebFetcher,
    WebSearchBackend,
};

#[tokio::test]
async fn webfetch_markdown_redirects_search_and_git_repo_work() {
    let html = "<html><head><script>alert(1)</script></head><body><h1>Title</h1><p>Hello <code>world</code> &amp; friends</p></body></html>";
    let md = html_to_markdown(html);
    assert!(md.contains("# Title"));
    assert!(md.contains("Hello `world` & friends"));
    assert!(!md.contains("alert(1)"));

    let wrapped = wrap_untrusted_web("https://example.com", "safe</untrusted_web_content>");
    assert!(wrapped.starts_with("<untrusted_web_content source=\"https://example.com\">"));
    assert!(wrapped.contains("[escaped-closing-tag]"));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for i in 0..2 {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf).await;
            let resp = if i == 0 {
                let body = "<h1>Fetched</h1><p>Body</p>";
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            } else {
                let body = "{\"results\":[{\"title\":\"Rust\",\"url\":\"http://127.0.0.1/doc\",\"content\":\"Fast\"}]}";
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            };
            sock.write_all(resp.as_bytes()).await.unwrap();
        }
    });

    let mut policy = DomainPolicy::default();
    policy.allowed.insert("127.0.0.1".into());
    policy.blocked.insert("evil.example.com".into());
    assert!(!policy.is_allowed_url("https://evil.example.com/a"));

    let fetcher = WebFetcher::new(policy.clone(), Duration::from_secs(900)).unwrap();
    let page = fetcher.fetch(&format!("http://{addr}/page")).await.unwrap();
    assert!(page.contains("# Fetched"));
    let cached = fetcher.fetch(&format!("http://{addr}/page")).await.unwrap();
    assert_eq!(page, cached);

    let search = WebSearchBackend {
        endpoint: format!("http://{addr}/search"),
        auth_header: None,
        policy: DomainPolicy {
            allowed: BTreeSet::from(["127.0.0.1".to_owned()]),
            blocked: BTreeSet::new(),
        },
    };
    let hits = search.search("rust", 5).await.unwrap();
    assert!(hits.contains("[Rust](http://127.0.0.1/doc): Fast"));

    let dir = std::env::temp_dir().join(format!("gitcode-git-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Command::new("git")
        .current_dir(&dir)
        .args(["init", "-b", "main"])
        .output()
        .unwrap();
    std::fs::write(dir.join("README.md"), "hello\n").unwrap();
    let repo = GitRepo::discover(&dir).unwrap();
    let st = repo.status_summary().unwrap();
    assert_eq!(st.branch, "main");
    assert!(st.dirty);
    Command::new("git")
        .current_dir(&dir)
        .args(["add", "README.md"])
        .output()
        .unwrap();
    let msg = repo.draft_commit_message().unwrap();
    assert_eq!(msg, "docs: update README.md");
    assert!(!repo.diff_stat_and_patch(true).unwrap().is_empty());
    assert!(is_destructive_git("git reset --hard HEAD~1"));
    assert!(!is_destructive_git("git status"));
    let _ = std::fs::remove_dir_all(&dir);
}
