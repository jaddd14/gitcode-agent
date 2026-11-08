//! Self-updater security checks use only a loopback server and temporary directories.
#![allow(clippy::unwrap_used)]

#[path = "../src/update.rs"]
#[allow(dead_code)]
mod update;

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl Environment {
    fn set(values: &[(&'static str, Option<&str>)]) -> Self {
        let previous = values
            .iter()
            .map(|(key, value)| {
                let old = std::env::var_os(key);
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
                (*key, old)
            })
            .collect();
        Self(previous)
    }
}
impl Drop for Environment {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", _) => "x86_64-pc-windows-msvc",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", _) => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        _ => "x86_64-unknown-linux-gnu",
    }
}

fn rejection(archive: Vec<u8>, hash: Option<&str>, manifest_version: &str) -> String {
    let _lock = ENV_LOCK.lock().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hash = hash
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{:x}", Sha256::digest(&archive)));
    let manifest = serde_json::to_vec(&serde_json::json!({
        "version": manifest_version, "date": "2026-11-08",
        "platforms": {target(): {"url": format!("{base}/archive"), "sha256": hash}}
    }))
    .unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_stop = stop.clone();
    let server = std::thread::spawn(move || {
        while !server_stop.load(std::sync::atomic::Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket
                        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                        .unwrap();
                    let mut request = [0; 4096];
                    let len = socket.read(&mut request).unwrap();
                    let request = String::from_utf8_lossy(&request[..len]);
                    let body = if request.starts_with("GET /latest.json ") {
                        &manifest
                    } else {
                        &archive
                    };
                    write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .unwrap();
                    let _ = socket.write_all(body);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(2))
                }
                Err(e) => panic!("{e}"),
            }
        }
    });
    let _env = Environment::set(&[("GITCODE_BASE_URL", Some(&base)), ("GITCODE_VERSION", None)]);
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(update::run());
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    server.join().unwrap();
    result.unwrap_err()
}

#[test]
fn hash_mismatch_keeps_current_executable() {
    let exe = std::env::current_exe().unwrap();
    let before = std::fs::read(&exe).unwrap();
    assert!(
        rejection(b"not an archive".to_vec(), Some(&"0".repeat(64)), "999.1.0")
            .contains("SHA256 не совпадает")
    );
    assert_eq!(before, std::fs::read(exe).unwrap());
}

#[test]
fn unsafe_manifest_versions_are_rejected() {
    for version in [
        "../evil",
        "1/../../evil",
        "1..2",
        "1\n2",
        "1%2f2",
        "v",
        "1;evil",
    ] {
        assert!(rejection(vec![], None, version).contains("версия"));
    }
}

#[test]
fn pinned_version_and_remote_url_validation() {
    let _lock = ENV_LOCK.lock().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for version in ["../evil", "1/evil", "1..2", "1\n2"] {
        let _env = Environment::set(&[("GITCODE_VERSION", Some(version))]);
        assert!(runtime
            .block_on(update::run())
            .unwrap_err()
            .contains("версия"));
    }
    for url in [
        "file:///etc/passwd",
        "http://example.com",
        "https://user:pass@example.com",
        "https://example.com/#fragment",
        "https://example.com/?query=1",
    ] {
        let _env = Environment::set(&[("GITCODE_BASE_URL", Some(url)), ("GITCODE_VERSION", None)]);
        assert!(runtime.block_on(update::run()).is_err());
    }
}

fn archive_with_path(path: &str, link: bool, duplicate: bool) -> Vec<u8> {
    if cfg!(windows) {
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        if link {
            archive
                .add_symlink("gitcode.exe", "elsewhere", options)
                .unwrap();
        } else {
            archive.start_file(path, options).unwrap();
            archive.write_all(b"binary").unwrap();
        }
        return archive.finish().unwrap().into_inner();
    }
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    for _ in 0..if duplicate { 2 } else { 1 } {
        let mut header = tar::Header::new_gnu();
        // Raw header allows malicious member names that tar::Builder::append_data rejects.
        header.as_mut_bytes()[..100].fill(0);
        header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
        header.set_size(if link { 0 } else { 6 });
        header.set_mode(0o755);
        if link {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_link_name("elsewhere").unwrap();
        }
        header.set_cksum();
        archive
            .append(&header, if link { &b""[..] } else { &b"binary"[..] })
            .unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap()
}

#[test]
fn archive_paths_and_links_cannot_escape() {
    let temp = tempfile::tempdir().unwrap();
    let sentinel = temp.path().join("sentinel");
    std::fs::write(&sentinel, b"untouched").unwrap();
    for path in [
        "../gitcode",
        "/gitcode",
        "nested/../../gitcode",
        "C:\\gitcode.exe",
        "nested/gitcode",
    ] {
        let error = rejection(archive_with_path(path, false, false), None, "999.1.0");
        assert!(
            error.contains("путь") || error.contains("нет gitcode"),
            "{error}"
        );
    }
    let name = if cfg!(windows) {
        "gitcode.exe"
    } else {
        "gitcode"
    };
    assert!(rejection(archive_with_path(name, true, false), None, "999.1.0").contains("обычный"));
    if !cfg!(windows) {
        assert!(
            rejection(archive_with_path(name, false, true), None, "999.1.0")
                .contains("Неоднозначный")
        );
    }
    assert_eq!(std::fs::read(sentinel).unwrap(), b"untouched");
}
