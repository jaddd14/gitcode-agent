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
        Self(path)
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
    let data = std::fs::read_to_string(&log).unwrap();
    assert!(!data.contains("test-sensitive-credential"));
    assert!(data.contains("[REDACTED]"));
    assert!(serde_json::from_str::<serde_json::Value>(data.trim()).is_ok());
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
