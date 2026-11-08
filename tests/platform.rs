//! Native platform validation independent of the invoking user's configuration.
#![allow(clippy::unwrap_used)]

use gitcode::platform::{self, Shell, ShellKind};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[test]
fn native_home_uses_only_native_variable() {
    let env = BTreeMap::from([
        ("HOME".into(), "/unix-home".into()),
        ("USERPROFILE".into(), "C:\\Users\\test".into()),
    ]);
    #[cfg(windows)]
    assert_eq!(
        platform::home_dir_from(&env),
        Some(PathBuf::from("C:\\Users\\test"))
    );
    #[cfg(unix)]
    assert_eq!(
        platform::home_dir_from(&env),
        Some(PathBuf::from("/unix-home"))
    );
    assert_eq!(platform::home_dir_from(&BTreeMap::new()), None);
}

#[test]
fn rejects_devices_streams_and_ambiguous_names() {
    for value in [
        "CON",
        "nul.txt",
        "a/COM1.log",
        "Lpt9",
        "COM¹",
        "a:stream",
        "a/trailing.",
        "a/trailing ",
        "a/<bad>",
    ] {
        assert!(
            platform::validate_windows_path(Path::new(value)).is_err(),
            "{value}"
        );
    }
    for value in [
        r"C:\work\file.txt",
        r"\\?\C:\work\file.txt",
        r"\\?\UNC\server\share\file.txt",
        "normal/COM10.txt",
    ] {
        assert!(
            platform::validate_windows_path(Path::new(value)).is_ok(),
            "{value}"
        );
    }
}

#[test]
fn completion_frames_preserve_native_dialects() {
    for kind in [
        ShellKind::Bash,
        ShellKind::Pwsh,
        ShellKind::PowerShell,
        ShellKind::Cmd,
    ] {
        let shell = Shell {
            kind,
            executable: PathBuf::new(),
        };
        let script = shell.script("echo 'literal'", 7);
        assert!(script.contains("M77:"));
        assert!(!shell.description().is_empty());
    }
}

#[test]
fn native_parser_cannot_hide_components() {
    use gitcode::tools::shell::split_native_commands;
    assert_eq!(
        split_native_commands(
            "Set-Location 'C:\\work dir'; Write-Output ok",
            ShellKind::Pwsh
        )
        .unwrap()
        .len(),
        2
    );
    assert!(split_native_commands("echo ok & del file", ShellKind::Cmd).is_err());
    assert!(split_native_commands("Write-Output \"$env:KEY\"", ShellKind::Pwsh).is_err());
    assert!(split_native_commands("echo %KEY%", ShellKind::Cmd).is_err());
}

#[cfg(windows)]
#[test]
fn windows_boundary_is_component_aware() {
    assert!(platform::path_within(
        Path::new(r"C:\WORK\src\file"),
        Path::new(r"c:\work")
    ));
    assert!(!platform::path_within(
        Path::new(r"C:\workspace\file"),
        Path::new(r"c:\work")
    ));
    assert!(platform::detect_shell("not-a-shell").is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn native_sessions_persist_and_recover_after_timeout() {
    use gitcode::{
        perm::{Engine, Mode},
        tools::shell::Session,
    };
    use std::time::Duration;
    let root = std::env::temp_dir().join(format!("gitcode-native-shell-{}", std::process::id()));
    std::fs::create_dir_all(root.join("sub")).unwrap();
    let engine = Engine::new(&root, Mode::Bypass).unwrap();
    for selection in ["bash", "pwsh", "powershell", "cmd"] {
        if platform::detect_shell(selection).is_err() {
            continue;
        }
        let session = Session::with_shell(&engine, &root, selection).unwrap();
        let (change, show, slow) = match selection {
            "bash" => ("cd sub", "pwd", "sleep 10"),
            "cmd" => ("cd sub", "cd", "ping -n 10 127.0.0.1"),
            _ => (
                "Set-Location sub",
                "Get-Location",
                "Start-Sleep -Seconds 10",
            ),
        };
        let first = session
            .execute(&engine, change, Duration::from_secs(15))
            .await
            .unwrap();
        assert_eq!(first.exit_code, Some(0), "{selection}: {}", first.output);
        let output = session
            .execute(&engine, show, Duration::from_secs(15))
            .await
            .unwrap();
        assert!(
            output.output.contains("sub"),
            "{selection}: {}",
            output.output
        );
        let output = session
            .execute(&engine, slow, Duration::from_millis(100))
            .await
            .unwrap();
        assert!(output.timed_out, "{selection}");
        let output = session
            .execute(&engine, "echo recovered", Duration::from_secs(15))
            .await
            .unwrap();
        assert!(output.output.contains("recovered"));
        session.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}
