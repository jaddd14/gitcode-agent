//! Verified, bounded self-updates without executing downloaded scripts.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::path::Path;
use std::time::Duration;

/// Canonical installer origin; installer rendering reads this default.
pub const INSTALL_BASE_URL: &str = "https://api.samp.team/gitcode";
const GITHUB_API: &str = "https://api.github.com/repos/jaddd14/gitcode-agent/releases";
const METADATA_LIMIT: usize = 2 * 1024 * 1024;
const ARCHIVE_LIMIT: usize = 128 * 1024 * 1024;
const BINARY_LIMIT: usize = 256 * 1024 * 1024;

#[derive(Deserialize)]
struct Manifest {
    version: String,
    platforms: HashMap<String, Platform>,
    #[serde(default, alias = "body")]
    changelog: String,
}

#[derive(Deserialize)]
struct Platform {
    url: String,
    sha256: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

struct Candidate {
    version: String,
    url: String,
    sha256: String,
    changelog: String,
}

fn version(value: &str) -> Result<&str, String> {
    let value = value.strip_prefix('v').unwrap_or(value);
    if value.len() > 128
        || !value.as_bytes().first().is_some_and(u8::is_ascii_digit)
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b))
        || value.contains("..")
    {
        return Err("Некорректная версия релиза".into());
    }
    Ok(value)
}

pub(crate) fn should_update(
    installed: &str,
    available: &str,
    pinned: bool,
) -> Result<bool, String> {
    let installed =
        semver::Version::parse(installed).map_err(|_| "Некорректная установленная версия")?;
    let available =
        semver::Version::parse(available).map_err(|_| "Некорректная версия обновления")?;
    Ok(available > installed || pinned && available != installed)
}

fn checked_url(value: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(value).map_err(|_| "Некорректный URL обновления")?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "Обновление допускает только HTTPS (HTTP — только loopback) без credentials/fragment"
                .into(),
        );
    }
    Ok(url)
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("gitcode-update/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("Too many redirects")
            } else if attempt.url().scheme() != "https"
                || checked_url(attempt.url().as_str()).is_err()
            {
                attempt.error("Unsafe update redirect")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|e| e.to_string())
}

async fn fetch(client: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let mut response = client
        .get(checked_url(url)?)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err("Файл обновления превышает допустимый размер".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err("Файл обновления превышает допустимый размер".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn target() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        _ => Err("Для этой платформы нет готового обновления".into()),
    }
}

fn archive_name(target: &str) -> String {
    format!(
        "gitcode-{target}.{}",
        if target.contains("windows") {
            "zip"
        } else {
            "tar.gz"
        }
    )
}

fn checksum(bytes: &[u8], expected: &str) -> Result<(), String> {
    if expected.len() != 64 || !expected.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("Некорректная SHA256".into());
    }
    if !format!("{:x}", Sha256::digest(bytes)).eq_ignore_ascii_case(expected) {
        return Err("SHA256 не совпадает; установленный файл не изменён".into());
    }
    Ok(())
}

fn checksum_entry(text: &[u8], name: &str) -> Result<String, String> {
    let text = std::str::from_utf8(text).map_err(|_| "Некорректный SHA256SUMS")?;
    let mut found = None;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hash), Some(file), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err("Некорректная строка SHA256SUMS".into());
        };
        if hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err("Некорректная SHA256".into());
        }
        if file.trim_start_matches('*') == name && found.replace(hash.to_owned()).is_some() {
            return Err("Дублирующая SHA256".into());
        }
    }
    found.ok_or_else(|| "В SHA256SUMS нет архива платформы".into())
}

fn asset<'a>(release: &'a Release, name: &str) -> Result<&'a str, String> {
    let mut matches = release.assets.iter().filter(|a| a.name == name);
    let found = matches
        .next()
        .ok_or_else(|| format!("В релизе нет {name}"))?;
    if matches.next().is_some() {
        return Err(format!("Дублирующий asset: {name}"));
    }
    Ok(&found.browser_download_url)
}

async fn github(
    client: &reqwest::Client,
    pin: Option<&str>,
    target: &str,
) -> Result<Candidate, String> {
    let api = std::env::var("GITCODE_GITHUB_API_URL").unwrap_or_else(|_| GITHUB_API.into());
    checked_url(&api)?;
    let api = api.trim_end_matches('/');
    let endpoint = pin.map_or_else(|| format!("{api}/latest"), |v| format!("{api}/tags/v{v}"));
    let release: Release = serde_json::from_slice(&fetch(client, &endpoint, METADATA_LIMIT).await?)
        .map_err(|e| format!("Некорректный ответ GitHub: {e}"))?;
    let released = version(&release.tag_name)?.to_owned();
    if pin.is_some_and(|pin| pin != released) {
        return Err("GitHub вернул другую версию".into());
    }
    let normal = archive_name(target);
    let legacy = format!(
        "gitcode-v{released}-{target}.{}",
        if target.contains("windows") {
            "zip"
        } else {
            "tar.gz"
        }
    );
    let names: Vec<_> = [normal.as_str(), legacy.as_str()]
        .into_iter()
        .filter(|name| release.assets.iter().any(|a| a.name == *name))
        .collect();
    if names.len() != 1 {
        return Err("Релиз должен содержать ровно один архив платформы".into());
    }
    let url = asset(&release, names[0])?.to_owned();
    let sums = fetch(client, asset(&release, "SHA256SUMS")?, METADATA_LIMIT).await?;
    Ok(Candidate {
        version: released,
        url,
        sha256: checksum_entry(&sums, names[0])?,
        changelog: release.body.unwrap_or_default(),
    })
}

async fn resolve(
    client: &reqwest::Client,
    base: &str,
    pin: Option<&str>,
    target: &str,
) -> Result<Candidate, String> {
    let base = base.trim_end_matches('/');
    let base_url = checked_url(base)?;
    if base_url.query().is_some() {
        return Err("Base URL не должен содержать query".into());
    }
    if let Some(pin) = pin {
        let name = archive_name(target);
        return match fetch(client, &format!("{base}/v{pin}/SHA256SUMS"), METADATA_LIMIT).await {
            Ok(sums) => Ok(Candidate {
                version: pin.into(),
                url: format!("{base}/v{pin}/{name}"),
                sha256: checksum_entry(&sums, &name)?,
                changelog: String::new(),
            }),
            Err(_) => github(client, Some(pin), target).await,
        };
    }
    let bytes = match fetch(client, &format!("{base}/latest.json"), METADATA_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => return github(client, None, target).await,
    };
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| format!("Некорректный latest.json: {e}"))?;
    let released = version(&manifest.version)?.to_owned();
    let platform = manifest
        .platforms
        .get(target)
        .ok_or("В latest.json нет текущей платформы")?;
    checked_url(&platform.url)?;
    Ok(Candidate {
        version: released,
        url: platform.url.clone(),
        sha256: platform.sha256.clone(),
        changelog: manifest.changelog,
    })
}

fn binary_name(windows: bool) -> &'static str {
    if windows {
        "gitcode.exe"
    } else {
        "gitcode"
    }
}

fn read_binary(reader: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(BINARY_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.is_empty() || bytes.len() > BINARY_LIMIT {
        return Err("Пустой или слишком большой исполняемый файл".into());
    }
    Ok(bytes)
}

fn extract(bytes: &[u8], windows: bool) -> Result<Vec<u8>, String> {
    let name = binary_name(windows);
    let mut binary = None;
    if windows {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
        if archive.len() > 128 {
            return Err("Слишком много файлов в архиве".into());
        }
        for i in 0..archive.len() {
            let file = archive.by_index(i).map_err(|e| e.to_string())?;
            let member = file.name().map_err(|error| error.to_string())?;
            safe_member(member.as_bytes())?;
            if member == name || member == format!("./{name}") {
                if !file.is_file()
                    || file
                        .unix_mode()
                        .is_some_and(|mode| mode & 0o170000 != 0 && mode & 0o170000 != 0o100000)
                    || binary.is_some()
                {
                    return Err("Неоднозначный или не обычный исполняемый файл в ZIP".into());
                }
                binary = Some(read_binary(file)?);
            }
        }
    } else {
        let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
        let mut archive = tar::Archive::new(decoder.take(BINARY_LIMIT as u64 + 1024 * 1024));
        for (count, file) in archive.entries().map_err(|e| e.to_string())?.enumerate() {
            if count >= 128 {
                return Err("Слишком много файлов в архиве".into());
            }
            let file = file.map_err(|e| e.to_string())?;
            let path = file.path_bytes();
            safe_member(&path)?;
            if path.as_ref() == name.as_bytes() || path.as_ref() == format!("./{name}").as_bytes() {
                if !file.header().entry_type().is_file() || binary.is_some() {
                    return Err("Неоднозначный или не обычный исполняемый файл в tar".into());
                }
                binary = Some(read_binary(file)?);
            }
        }
    }
    binary.ok_or_else(|| format!("В корне архива нет {name}"))
}

fn safe_member(path: &[u8]) -> Result<(), String> {
    let path = std::str::from_utf8(path).map_err(|_| "Некорректный путь в архиве")?;
    if path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path.chars().any(char::is_control)
        || path.split('/').any(|part| part == "..")
    {
        return Err("Небезопасный путь в архиве".into());
    }
    Ok(())
}

fn replace(executable: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = executable.parent().ok_or("Не найден каталог программы")?;
    let metadata = std::fs::symlink_metadata(executable).map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("Программа не является обычным файлом".into());
    }
    let mut staged = tempfile::Builder::new()
        .prefix(".gitcode-update-")
        .tempfile_in(parent)
        .map_err(|e| e.to_string())?;
    staged.write_all(bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staged
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(
                metadata.permissions().mode() | 0o100,
            ))
            .map_err(|e| e.to_string())?;
    }
    staged.as_file().sync_all().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        let old = executable.with_extension("exe.old");
        if old.try_exists().map_err(|e| e.to_string())? {
            return Err("Остался предыдущий .old; перезапустите gitcode перед обновлением".into());
        }
        std::fs::rename(executable, &old).map_err(|e| e.to_string())?;
        if let Err(error) = staged.persist(executable) {
            return match std::fs::rename(&old, executable) {
                Ok(()) => Err(format!(
                    "Замена не удалась, предыдущая версия восстановлена: {}",
                    error.error
                )),
                Err(rollback) => Err(format!(
                    "Замена не удалась: {}; восстановите {} вручную: {rollback}",
                    error.error,
                    old.display()
                )),
            };
        }
    }
    #[cfg(not(windows))]
    staged
        .persist(executable)
        .map_err(|e| e.error.to_string())?;
    Ok(())
}

/// Remove only the backup of the currently running Windows gitcode executable.
pub fn cleanup_old() {
    #[cfg(windows)]
    if let Ok(executable) = std::env::current_exe() {
        if executable
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("gitcode.exe"))
        {
            let old = executable.with_extension("exe.old");
            if std::fs::symlink_metadata(&old)
                .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
            {
                let _ = std::fs::remove_file(old);
            }
        }
    }
}

/// Update the current executable after authenticating the archive's SHA256.
/// `GITCODE_BASE_URL` overrides the primary origin; `GITCODE_VERSION` pins a release.
pub async fn run() -> Result<(), String> {
    let pin = std::env::var("GITCODE_VERSION").ok();
    let pin = pin.as_deref().map(version).transpose()?;
    let base = std::env::var("GITCODE_BASE_URL").unwrap_or_else(|_| INSTALL_BASE_URL.into());
    let client = client()?;
    let target = target()?;
    let mut candidate = resolve(&client, &base, pin, target).await?;
    if candidate.version == env!("CARGO_PKG_VERSION") {
        println!("gitcode {} уже установлен.", candidate.version);
        return Ok(());
    }
    if !should_update(env!("CARGO_PKG_VERSION"), &candidate.version, pin.is_some())? {
        println!(
            "Установленная версия {} новее опубликованной {}.",
            env!("CARGO_PKG_VERSION"),
            candidate.version
        );
        return Ok(());
    }
    let archive = match fetch(&client, &candidate.url, ARCHIVE_LIMIT).await {
        Ok(bytes) => bytes,
        Err(primary_error) => {
            candidate = github(&client, Some(&candidate.version), target)
                .await
                .map_err(|fallback| {
                    format!("Основной сервер: {primary_error}; GitHub: {fallback}")
                })?;
            fetch(&client, &candidate.url, ARCHIVE_LIMIT).await?
        }
    };
    checksum(&archive, &candidate.sha256)?;
    let binary = extract(&archive, target.contains("windows"))?;
    if candidate.changelog.is_empty() {
        // Release notes are optional; unavailable notes never block a verified update.
        let endpoint = format!("{GITHUB_API}/tags/v{}", candidate.version);
        if let Ok(bytes) = fetch(&client, &endpoint, METADATA_LIMIT).await {
            if let Ok(release) = serde_json::from_slice::<Release>(&bytes) {
                if version(&release.tag_name).ok() == Some(candidate.version.as_str()) {
                    candidate.changelog = release.body.unwrap_or_default();
                }
            }
        }
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    replace(&executable, &binary)?;
    println!(
        "gitcode обновлён до {}. Перезапустите программу.",
        candidate.version
    );
    if !candidate.changelog.is_empty() {
        println!("Изменения:");
        for line in candidate.changelog.lines().take(12) {
            let safe: String = line.chars().filter(|c| !c.is_control()).take(240).collect();
            println!("{safe}");
        }
    } else {
        println!(
            "Описание релиза: https://github.com/jaddd14/gitcode-agent/releases/tag/v{}",
            candidate.version
        );
    }
    Ok(())
}
