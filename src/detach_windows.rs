//! Windows session persistence through an installed native psmux multiplexer.
use std::{
    fs,
    hash::{Hash, Hasher},
    path::Path,
    process::{Command, Stdio},
};

fn command(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    // A prewarmed server may carry another launch's environment and credentials.
    command.env("PSMUX_NO_WARM", "1");
    command
}

fn same_directory(left: &Path, right: &Path) -> bool {
    let normalized = |path: &Path| {
        fs::canonicalize(path)
            .ok()
            .map(|path| path.to_string_lossy().to_lowercase())
    };
    normalized(left)
        .zip(normalized(right))
        .is_some_and(|(a, b)| a == b)
}

fn hide_managed_status(multiplexer: &Path, target: &str) -> Result<(), String> {
    let status = command(multiplexer)
        .args(["set-option", "-t", target, "status", "off"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "не удалось настроить сессию GitCode")?;
    if status.success() {
        Ok(())
    } else {
        Err("не удалось скрыть строку psmux в сессии GitCode".into())
    }
}

/// Apply display settings before the managed worker draws its header.
pub fn prepare_psmux_worker() -> Result<(), String> {
    let multiplexer =
        gitcode::shell::find_executable("psmux.exe").ok_or("не удалось найти psmux")?;
    let session =
        std::env::var("PSMUX_SESSION").map_err(|_| "не удалось определить сессию psmux")?;
    hide_managed_status(&multiplexer, &session)
}

fn attach(
    multiplexer: &Path,
    target: &str,
    pane: Option<&str>,
    inside: bool,
) -> Result<bool, String> {
    if let Some(pane) = pane {
        let status = command(multiplexer)
            .args(["select-window", "-t", pane])
            .status()
            .map_err(|_| "не удалось выбрать окно GitCode")?;
        if !status.success() {
            return Err("не удалось выбрать окно GitCode".into());
        }
        let _ = command(multiplexer)
            .args(["select-pane", "-t", pane])
            .status();
    }
    let action = if inside {
        "switch-client"
    } else {
        "attach-session"
    };
    let status = command(multiplexer)
        .args([action, "-t", target])
        .status()
        .map_err(|_| "не удалось подключиться к сессии GitCode")?;
    if status.success() {
        Ok(true)
    } else {
        Err("не удалось подключиться к сессии GitCode; используй gitcode --continue".into())
    }
}

pub fn launch(continue_session: bool) -> Result<bool, String> {
    let inside = std::env::var_os("PSMUX_SESSION").is_some();
    if inside && !continue_session {
        return Ok(false);
    }
    let Some(multiplexer) = gitcode::shell::find_executable("psmux.exe") else {
        eprintln!("Для продолжения после закрытия терминала установи psmux: https://github.com/psmux/psmux");
        return Ok(false);
    };
    let root = std::env::current_dir()
        .and_then(fs::canonicalize)
        .map_err(|_| "не удалось определить рабочий каталог")?;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    root.to_string_lossy().to_lowercase().hash(&mut hash);
    let prefix = format!("gitcode-{:016x}-", hash.finish());
    let list = match command(&multiplexer)
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("Для продолжения после закрытия терминала установи psmux: https://github.com/psmux/psmux");
            return Ok(false);
        }
        Err(_) => return Err("не удалось запустить psmux".into()),
    };
    let version = command(&multiplexer)
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            if !output.status.success() {
                return None;
            }
            let text = String::from_utf8(output.stdout).ok()?;
            let mut words = text.split_whitespace();
            if words.next()? != "psmux" {
                return None;
            }
            semver::Version::parse(words.next()?).ok()
        });
    if !version.is_some_and(|version| version >= semver::Version::new(3, 4, 0)) {
        eprintln!("Для фоновой сессии нужна psmux версии 3.4.0 или новее.");
        return Ok(false);
    }
    if continue_session && list.status.success() {
        let sessions = String::from_utf8_lossy(&list.stdout);
        if let Some(name) = sessions.lines().rfind(|name| name.starts_with(&prefix)) {
            hide_managed_status(&multiplexer, name)?;
            return attach(&multiplexer, name, None, inside);
        }
        if let Ok(panes) = command(&multiplexer)
            .args([
                "list-panes",
                "-a",
                "-F",
                "#{pane_id}\t#{pane_current_path}\t#{pane_current_command}\t#{session_name}",
            ])
            .output()
        {
            let executable = std::env::current_exe()
                .ok()
                .and_then(|path| path.file_name().map(|v| v.to_string_lossy().to_lowercase()));
            let panes = String::from_utf8_lossy(&panes.stdout);
            if let Some((pane, session)) = panes
                .lines()
                .filter_map(|line| {
                    let mut fields = line.split('\t');
                    let (pane, path, program, session) = (
                        fields.next()?,
                        fields.next()?,
                        fields.next()?,
                        fields.next()?,
                    );
                    let program = program.to_lowercase();
                    let same_executable = executable.as_ref().is_some_and(|name| {
                        name == &program || name.strip_suffix(".exe") == Some(program.as_str())
                    });
                    (same_executable && same_directory(Path::new(path), &root))
                        .then_some((pane, session))
                })
                .next_back()
            {
                return attach(&multiplexer, session, Some(pane), inside);
            }
        }
    }
    if inside {
        return Ok(false);
    }
    let name = format!("{prefix}{}", std::process::id());
    let executable = std::env::current_exe().map_err(|_| "не удалось определить путь к GitCode")?;
    let mode = gitcode::theme::detect(
        true,
        std::env::var_os("NO_COLOR").is_some(),
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    );
    let color = match mode {
        gitcode::theme::ColorMode::TrueColor => "truecolor",
        gitcode::theme::ColorMode::Indexed256 => "256",
        gitcode::theme::ColorMode::Ansi16 => "ansi16",
        gitcode::theme::ColorMode::None => "none",
    };
    let mut create = command(&multiplexer);
    create
        .args(["new-session", "-d", "-s", &name, "-c"])
        .arg(&root);
    create.args(["-e", &format!("GITCODE_PSMUX_COLOR_MODE={color}")]);
    if let Ok((width, height)) = crossterm::terminal::size() {
        if width > 0 && height > 0 {
            create.args(["-x", &width.to_string(), "-y", &height.to_string()]);
        }
    }
    let status = create
        .arg("--")
        .arg(executable)
        // An extra argument makes psmux execute argv directly, without a shell.
        .arg("--gitcode-psmux-worker")
        .args(std::env::args_os().skip(1))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "не удалось создать фоновую сессию GitCode")?;
    if !status.success() {
        return Err("не удалось создать фоновую сессию GitCode в psmux".into());
    }
    if let Err(error) = hide_managed_status(&multiplexer, &name) {
        let _ = command(&multiplexer)
            .args(["kill-session", "-t", &name])
            .status();
        return Err(error);
    }
    attach(&multiplexer, &name, None, false)
}
