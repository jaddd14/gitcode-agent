//! Keep an interactive session alive across terminal disconnects using tmux.
#[cfg(unix)]
mod unix {
    use std::{
        ffi::OsString,
        fs::{self, OpenOptions},
        io::{Read, Write},
        os::unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
        path::{Path, PathBuf},
        process::{Command, Stdio},
    };

    const MARKER: &str = "GITCODE_TMUX_MANAGED";
    const ENV_FILE: &str = "GITCODE_LAUNCH_ENV";

    fn directory(home: &Path) -> Result<PathBuf, String> {
        let path = home.join(".gitcode");
        let mut dir = fs::DirBuilder::new();
        use std::os::unix::fs::DirBuilderExt;
        dir.mode(0o700);
        match dir.create(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err("не удалось создать каталог GitCode".into()),
        }
        let metadata =
            fs::symlink_metadata(&path).map_err(|_| "не удалось проверить каталог GitCode")?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err("каталог GitCode должен быть приватным".into());
        }
        Ok(path)
    }

    // Restore the launcher's complete environment before starting any runtime threads.
    pub fn restore_environment() -> Result<(), String> {
        if std::env::var_os(MARKER).is_none() {
            return Ok(());
        }
        let Some(file) = std::env::var_os(ENV_FILE) else {
            return Ok(());
        };
        std::env::remove_var(ENV_FILE);
        let path = PathBuf::from(file);
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("не задан HOME")?;
        let dir = directory(&home)?;
        if path.parent() != Some(dir.as_path())
            || !path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("launch-env-") && s.ends_with(".json"))
        {
            return Err("неверный путь окружения GitCode".into());
        }
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = options
            .open(&path)
            .map_err(|_| "не удалось открыть окружение GitCode")?;
        let meta = file
            .metadata()
            .map_err(|_| "не удалось проверить окружение GitCode")?;
        if !meta.is_file()
            || meta.permissions().mode() & 0o077 != 0
            || meta.nlink() != 1
            || meta.len() > 4 * 1024 * 1024
        {
            return Err("небезопасный файл окружения GitCode".into());
        }
        let mut bytes = Vec::new();
        file.take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "не удалось прочитать окружение GitCode")?;
        fs::remove_file(&path).map_err(|_| "не удалось удалить временное окружение GitCode")?;
        let vars: Vec<(Vec<u8>, Vec<u8>)> =
            serde_json::from_slice(&bytes).map_err(|_| "неверное окружение GitCode")?;
        for (key, _) in std::env::vars_os() {
            let bytes = key.as_os_str().as_bytes();
            if bytes != b"TERM"
                && bytes != b"TMUX"
                && bytes != b"TMUX_PANE"
                && bytes != MARKER.as_bytes()
            {
                std::env::remove_var(key);
            }
        }
        for (key, value) in vars {
            if key == b"TERM"
                || key == b"TMUX"
                || key == b"TMUX_PANE"
                || key == MARKER.as_bytes()
                || key == ENV_FILE.as_bytes()
                || key.contains(&0)
                || key.contains(&b'=')
                || value.contains(&0)
                || key.is_empty()
            {
                continue;
            }
            std::env::set_var(OsString::from_vec(key), OsString::from_vec(value));
        }
        Ok(())
    }

    fn env_file(home: &Path) -> Result<PathBuf, String> {
        let dir = directory(home)?;
        let vars: Vec<(Vec<u8>, Vec<u8>)> = std::env::vars_os()
            .map(|(k, v)| {
                (
                    k.as_os_str().as_bytes().to_vec(),
                    v.as_os_str().as_bytes().to_vec(),
                )
            })
            .collect();
        let data =
            serde_json::to_vec(&vars).map_err(|_| "не удалось сохранить окружение GitCode")?;
        if data.len() > 4 * 1024 * 1024 {
            return Err("слишком большое окружение GitCode".into());
        }
        for attempt in 0..100 {
            let path = dir.join(format!("launch-env-{}-{attempt}.json", std::process::id()));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            match options.open(&path) {
                Ok(mut file) => {
                    if file.write_all(&data).is_err() {
                        let _ = fs::remove_file(&path);
                        return Err("не удалось сохранить окружение GitCode".into());
                    }
                    return Ok(path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err("не удалось сохранить окружение GitCode".into()),
            }
        }
        Err("не удалось создать файл окружения GitCode".into())
    }

    fn attach(target: &str, pane: Option<&str>, inside_tmux: bool) -> Result<bool, String> {
        if let Some(pane) = pane {
            let status = Command::new("tmux")
                .args(["select-window", "-t", pane])
                .status()
                .map_err(|_| "не удалось выбрать окно GitCode")?;
            if !status.success() {
                return Err("не удалось выбрать окно GitCode".into());
            }
            let _ = Command::new("tmux")
                .args(["select-pane", "-t", pane])
                .status();
        }
        let status = Command::new("tmux")
            .args([
                if inside_tmux {
                    "switch-client"
                } else {
                    "attach-session"
                },
                "-t",
                target,
            ])
            .status()
            .map_err(|_| "не удалось подключиться к сессии GitCode")?;
        if status.success() {
            Ok(true)
        } else {
            Err("не удалось подключиться к сессии GitCode".into())
        }
    }

    pub fn launch(continue_session: bool) -> Result<bool, String> {
        use std::hash::{Hash, Hasher};
        if std::env::var_os(MARKER).is_some() {
            return Ok(false);
        }
        let inside_tmux = std::env::var_os("TMUX").is_some();
        if inside_tmux && !continue_session {
            return Ok(false);
        }
        let root = std::env::current_dir()
            .and_then(fs::canonicalize)
            .map_err(|_| "не удалось определить рабочий каталог")?;
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        root.hash(&mut hash);
        let prefix = format!("gitcode-{:016x}-", hash.finish());
        let list = Command::new("tmux")
            .args([
                "list-sessions",
                "-F",
                "#{session_name}\t#{session_attached}",
            ])
            .output();
        let list = match list {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("Для продолжения после обрыва соединения установи tmux.");
                return Ok(false);
            }
            Err(_) => return Err("не удалось запустить tmux".into()),
        };
        if continue_session && list.status.success() {
            let sessions = String::from_utf8_lossy(&list.stdout);
            if let Some(name) = sessions
                .lines()
                .filter_map(|line| line.split_once('\t'))
                .filter(|(name, _)| name.starts_with(&prefix))
                .map(|(name, _)| name)
                .next_back()
            {
                return attach(name, None, inside_tmux);
            }
            // GitCode launched inside an existing tmux session has no managed
            // session prefix. Locate its live pane before starting a new process.
            let panes = Command::new("tmux")
                .args([
                    "list-panes",
                    "-a",
                    "-F",
                    "#{pane_id}\t#{pane_current_path}\t#{pane_current_command}\t#{session_name}",
                ])
                .output()
                .map_err(|_| "не удалось найти сессию GitCode")?;
            let executable = std::env::current_exe()
                .ok()
                .and_then(|p| p.file_name().map(|s| s.to_os_string()));
            let panes = String::from_utf8_lossy(&panes.stdout);
            if let Some((pane, session)) = panes
                .lines()
                .filter_map(|line| {
                    let mut fields = line.split('\t');
                    let (pane, path, command, session) = (
                        fields.next()?,
                        fields.next()?,
                        fields.next()?,
                        fields.next()?,
                    );
                    let same_project = fs::canonicalize(path).is_ok_and(|path| path == root);
                    (same_project && executable.as_deref() == Some(std::ffi::OsStr::new(command)))
                        .then_some((pane, session))
                })
                .next_back()
            {
                return attach(session, Some(pane), inside_tmux);
            }
        }
        if inside_tmux {
            return Ok(false);
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("не задан HOME")?;
        let path = env_file(&home)?;
        let name = format!("{prefix}{}", std::process::id());
        let executable =
            std::env::current_exe().map_err(|_| "не удалось определить путь к GitCode")?;
        let mut home_var = OsString::from("HOME=");
        home_var.push(&home);
        let mut launch_var = OsString::from(format!("{ENV_FILE}="));
        launch_var.push(&path);
        let result = Command::new("tmux")
            .arg("new-session")
            .arg("-d")
            .arg("-s")
            .arg(&name)
            .arg("-c")
            .arg(&root)
            .arg("-e")
            .arg(home_var)
            .arg("-e")
            .arg(format!("{MARKER}=1"))
            .arg("-e")
            .arg(launch_var)
            .arg(executable)
            .args(std::env::args_os().skip(1))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !result.is_ok_and(|status| status.success()) {
            let _ = fs::remove_file(path);
            return Err("не удалось создать фоновую сессию GitCode в tmux".into());
        }
        // Do not leave a credential-bearing launch file behind if the child fails
        // before reading it. The worker removes it immediately during startup.
        for _ in 0..500 {
            if !path.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if path.exists() {
            let _ = fs::remove_file(&path);
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", &name])
                .status();
            return Err("не удалось передать окружение фоновой сессии GitCode".into());
        }
        let status = Command::new("tmux")
            .args(["attach-session", "-t", &name])
            .status()
            .map_err(|_| "не удалось подключиться к сессии GitCode")?;
        if status.success() {
            Ok(true)
        } else {
            Err("не удалось подключиться к сессии GitCode; используй gitcode --continue".into())
        }
    }
}

#[cfg(unix)]
pub use unix::{launch, restore_environment};
#[cfg(not(unix))]
pub fn restore_environment() -> Result<(), String> {
    Ok(())
}
#[cfg(windows)]
#[path = "detach_windows.rs"]
mod windows;
#[cfg(windows)]
pub use windows::launch;
#[cfg(not(any(unix, windows)))]
pub fn launch(_: bool) -> Result<bool, String> {
    Ok(false)
}
