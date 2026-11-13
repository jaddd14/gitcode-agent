//! Persist the reasoning effort selected for each model.
use std::{collections::BTreeMap, io, path::Path};

fn path(home: &Path) -> std::path::PathBuf {
    home.join(".gitcode/effort.json")
}

fn read(home: &Path) -> io::Result<BTreeMap<String, String>> {
    match std::fs::read(path(home)) {
        Ok(data) => serde_json::from_slice(&data).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error),
    }
}

/// Return the saved level, or xhigh for a model without a preference.
/// The string `default` explicitly selects the provider's own default.
pub fn get(home: &Path, model: &str) -> io::Result<String> {
    Ok(read(home)?
        .get(model)
        .cloned()
        .unwrap_or_else(|| "xhigh".into()))
}

/// Save a model's explicitly selected level without changing other models.
pub fn set(home: &Path, model: &str, level: &str) -> io::Result<()> {
    if !matches!(
        level,
        "default" | "low" | "medium" | "high" | "xhigh" | "max"
    ) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid effort",
        ));
    }
    let mut values = read(home)?;
    values.insert(model.into(), level.into());
    let destination = path(home);
    let directory = destination
        .parent()
        .ok_or_else(|| io::Error::other("invalid path"))?;
    std::fs::create_dir_all(directory)?;
    let temporary = directory.join(format!(".effort.{}.tmp", std::process::id()));
    let result = (|| {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        serde_json::to_writer_pretty(&mut file, &values).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&temporary, &destination)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

/// Apply a saved preference to a client.
pub fn apply(
    client: &mut crate::provider::transport::Client,
    home: &Path,
    model: &str,
) -> io::Result<()> {
    let level = get(home, model)?;
    client
        .set_effort(if level == "default" {
            None
        } else {
            Some(&level)
        })
        .map_err(io::Error::other)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn remembers_independent_model_preferences_including_provider_default() {
        let home = std::env::temp_dir().join(format!(
            "gitcode-effort-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert_eq!(get(&home, "one").unwrap(), "xhigh");
        set(&home, "one", "low").unwrap();
        set(&home, "two", "default").unwrap();
        assert_eq!(get(&home, "one").unwrap(), "low");
        assert_eq!(get(&home, "two").unwrap(), "default");
        assert_eq!(get(&home, "three").unwrap(), "xhigh");
        let mut client = crate::provider::transport::Client::new(
            "http://localhost:9",
            None,
            false,
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        apply(&mut client, &home, "one").unwrap();
        assert_eq!(client.effort(), Some("low"));
        apply(&mut client, &home, "two").unwrap();
        assert_eq!(client.effort(), None);
        apply(&mut client, &home, "three").unwrap();
        assert_eq!(client.effort(), Some("xhigh"));
        std::fs::remove_dir_all(home).unwrap();
    }
}
