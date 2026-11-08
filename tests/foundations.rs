//! Foundation acceptance tests use isolated homes rather than process globals.
#![allow(clippy::unwrap_used)]

use gitcode::{
    config::{self, Overrides},
    credentials::{self, Secret},
    logging::Logger,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "gitcode-foundation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(std::fs::canonicalize(path).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn layers_environment_cli_and_unknown_keys() {
    let fixture = Fixture::new();
    let home = fixture.0.join("home");
    let project = fixture.0.join("project");
    std::fs::create_dir_all(home.join(".gitcode")).unwrap();
    std::fs::create_dir_all(project.join(".gitcode")).unwrap();
    std::fs::write(
        home.join(".gitcode/config.toml"),
        "model = 'user'\n[provider]\nname = 'user-provider'\n",
    )
    .unwrap();
    std::fs::write(project.join(".gitcode/config.toml"), "model = 'project'\n").unwrap();
    std::fs::write(
        project.join(".gitcode/config.local.toml"),
        "model = 'local'\n",
    )
    .unwrap();
    let empty = BTreeMap::new();
    assert_eq!(
        config::load(&home, &project, &empty, &Overrides::new())
            .unwrap()
            .model,
        "local"
    );
    let env = BTreeMap::from([("GITCODE_MODEL".into(), "environment".into())]);
    let c = config::load(&home, &project, &env, &Overrides::new()).unwrap();
    assert_eq!(c.model, "environment");
    assert_eq!(c.provider.name, "user-provider");
    let cli = BTreeMap::from([("model".into(), toml::Value::String("cli".into()))]);
    assert_eq!(
        config::load(&home, &project, &env, &cli).unwrap().model,
        "cli"
    );
    std::fs::write(
        project.join(".gitcode/config.local.toml"),
        "unknown_key = 'secret-example'\n",
    )
    .unwrap();
    let e = config::load(&home, &project, &empty, &Overrides::new())
        .unwrap_err()
        .to_string();
    assert!(!e.contains("secret-example"));
    assert!(e.contains("config.local.toml"));
}

#[test]
fn credential_resolution_prefers_environment_and_fails_closed() {
    let fixture = Fixture::new();
    let home = fixture.0.join("home");
    let store = home.join(".gitcode/credentials");
    std::fs::create_dir_all(&store).unwrap();
    let key = "TEST_PROVIDER_KEY";
    let path = store.join(key);
    assert!(credentials::resolve(&home, key, &BTreeMap::new())
        .unwrap()
        .is_none());
    credentials::create(&path, &Secret::new("stored-test-value".into())).unwrap();
    assert_eq!(
        credentials::resolve(&home, key, &BTreeMap::new())
            .unwrap()
            .unwrap()
            .expose(),
        "stored-test-value"
    );
    let env = BTreeMap::from([(key.to_owned(), "environment-test-value".to_owned())]);
    assert_eq!(
        credentials::resolve(&home, key, &env)
            .unwrap()
            .unwrap()
            .expose(),
        "environment-test-value"
    );
    let env = BTreeMap::from([(key.to_owned(), String::new())]);
    assert_eq!(
        credentials::resolve(&home, key, &env)
            .unwrap()
            .unwrap()
            .expose(),
        "stored-test-value"
    );
    assert!(credentials::resolve(&home, "../../outside", &BTreeMap::new()).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(credentials::resolve(&home, key, &BTreeMap::new()).is_err());
        assert_eq!(
            credentials::resolve(
                &home,
                key,
                &BTreeMap::from([(key.to_owned(), "env".to_owned())])
            )
            .unwrap()
            .unwrap()
            .expose(),
            "env"
        );
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(fixture.0.join("missing"), &path).unwrap();
        assert!(credentials::resolve(&home, key, &BTreeMap::new()).is_err());
    }
}

#[test]
fn executable_uses_stored_credentials_without_polluting_stdout() {
    let fixture = Fixture::new();
    let home = fixture.0.join("home");
    let project = fixture.0.join("project");
    let store = home.join(".gitcode/credentials");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::create_dir_all(project.join(".gitcode")).unwrap();
    std::fs::write(
        project.join(".gitcode/config.toml"),
        "[provider]\napi_key_env = 'ISOLATED_CREDENTIAL'\n",
    )
    .unwrap();
    credentials::create(
        &store.join("ISOLATED_CREDENTIAL"),
        &Secret::new("stored-executable-test-value".into()),
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_gitcode"))
        .args(["doctor", "--json"])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check["name"] == "credentials" && check["ok"] == true));
    let log = std::fs::read_to_string(home.join(".gitcode/gitcode.log")).unwrap();
    assert!(log.contains("application started"));
    assert!(log.contains("application finished"));
    for data in [&output.stdout[..], &output.stderr[..], log.as_bytes()] {
        assert!(!String::from_utf8_lossy(data).contains("stored-executable-test-value"));
    }
}

#[test]
fn executable_authenticates_with_saved_key_and_hides_reflected_errors() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    for fail in [false, true] {
        let fixture = Fixture::new();
        let home = fixture.0.join("home");
        let project = fixture.0.join("project");
        let store = home.join(".gitcode/credentials");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::create_dir_all(project.join(".gitcode")).unwrap();
        std::fs::write(
            project.join("GITCODE.md"),
            "workspace-memory-acceptance-marker",
        )
        .unwrap();
        let secret = "isolated-saved-http-credential";
        credentials::create(
            &store.join("ISOLATED_HTTP_KEY"),
            &Secret::new(secret.into()),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        std::fs::write(
            project.join(".gitcode/config.toml"),
            format!("[provider]\napi_key_env = 'ISOLATED_HTTP_KEY'\nbase_url = 'http://{address}'\ntimeout_seconds = 5\n"),
        )
        .unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "CLI never connected to mock");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("mock accept failed: {error}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let count = socket.read(&mut buffer).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        assert!(String::from_utf8_lossy(&request[end + 4..])
                            .contains("workspace-memory-acceptance-marker"));
                        assert!(
                            headers.lines().any(|line| {
                                line.split_once(':').is_some_and(|(name, value)| {
                                    name.eq_ignore_ascii_case("x-api-key") && value.trim() == secret
                                })
                            }),
                            "saved key absent from HTTP authentication"
                        );
                        break;
                    }
                }
            }
            let body = if fail {
                format!("{{\"error\":{{\"message\":\"reflected {secret}\"}}}}")
            } else {
                concat!(
                    "data: {\"type\":\"message_start\",\"message\":{}}\n\n",
                    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"verified saved credential\"}}\n\n",
                    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
                    "data: {\"type\":\"message_stop\"}\n\n"
                ).to_owned()
            };
            let status = if fail { "401 Unauthorized" } else { "200 OK" };
            write!(
                socket,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_gitcode"))
            .args([
                "-p",
                "check saved authentication",
                "--output-format",
                "stream-json",
            ])
            .env_clear()
            .env("HOME", &home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .current_dir(&project)
            .output()
            .unwrap();
        server.join().unwrap();
        assert_eq!(
            output.status.success(),
            !fail,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if !fail {
            let events: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(events[0]["type"], "init");
            assert!(events.iter().any(|event| event["type"] == "message_delta"
                && event["text"] == "verified saved credential"));
            assert_eq!(events.last().unwrap()["type"], "result");
            assert_eq!(events.last().unwrap()["text"], "verified saved credential");
            let sessions = gitcode::session::store::SessionStore::open(&home, &project).unwrap();
            let latest = sessions.list().unwrap();
            let saved = sessions.resume(&latest[0].id).unwrap();
            assert_eq!(saved.messages.len(), 2);
            let usage = gitcode::usage::UsageLedger::open(&home)
                .unwrap()
                .entries(None)
                .unwrap();
            assert_eq!(usage.len(), 1);
            assert_eq!(usage[0].session_id, saved.id);
            assert!(usage[0].cost_usd.is_none());
            assert_eq!(
                saved.messages[0],
                gitcode::provider::canonical::Message::text(
                    gitcode::provider::canonical::Role::User,
                    "check saved authentication"
                )
            );
        }
        let log = std::fs::read(home.join(".gitcode/gitcode.log")).unwrap();
        for data in [&output.stdout[..], &output.stderr[..], &log[..]] {
            assert!(!String::from_utf8_lossy(data).contains(secret));
        }
    }
}

#[test]
fn private_credentials_and_redacted_json_logs() {
    let fixture = Fixture::new();
    let path = fixture.0.join("credential");
    let secret = Secret::new("test-sensitive-credential".into());
    credentials::create(&path, &secret).unwrap();
    assert!(credentials::create(&path, &secret).is_err());
    assert_eq!(credentials::read(&path).unwrap().expose(), secret.expose());
    let log = fixture.0.join("log.jsonl");
    let mut logger = Logger::open(&log, vec![secret]).unwrap();
    logger
        .record("info", "key=test-sensitive-credential")
        .unwrap();
    logger.add_secret(Secret::new("additional-sensitive-value".into()));
    assert_eq!(logger.redact("additional-sensitive-value"), "[REDACTED]");
    logger
        .record("error", "additional-sensitive-value")
        .unwrap();
    let data = std::fs::read_to_string(&log).unwrap();
    assert!(!data.contains("test-sensitive-credential"));
    assert!(data.contains("[REDACTED]"));
    assert!(!data.contains("additional-sensitive-value"));
    assert!(data
        .lines()
        .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(credentials::read(&path).is_err());
    }
}
