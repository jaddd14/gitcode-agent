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
        if matches!(kind, ShellKind::Pwsh | ShellKind::PowerShell) {
            let formatter = script.find("| Out-Default;").unwrap();
            let marker = script.find("[Console]::Out.Write").unwrap();
            assert!(
                formatter < marker,
                "objects must be formatted before framing"
            );
            assert!(script.find("$__gc_ok=$?").unwrap() < formatter);
            assert!(script.contains("[Console]::Out.Flush()"));
        }
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
        if matches!(selection, "pwsh" | "powershell") {
            let output = session
                .execute(
                    &engine,
                    "Write-Output formatter-drained",
                    Duration::from_secs(15),
                )
                .await
                .unwrap();
            assert_eq!(output.exit_code, Some(0), "{selection}: {}", output.output);
            assert_eq!(output.output.trim(), "formatter-drained", "{selection}");
            let output = session
                .execute(
                    &engine,
                    "Get-Item missing-native-shell-regression-file",
                    Duration::from_secs(15),
                )
                .await
                .unwrap();
            assert_ne!(output.exit_code, Some(0), "{selection}: {}", output.output);
            assert!(!output.output.is_empty(), "{selection}: error output lost");
            // Exercise status at the user-script boundary, not just the outer
            // dot-source invocation. Final-command/pipeline status follows PS.
            for (command, expected, text) in [
                ("Write-Output 'кириллица'; throw 'terminating-regression'", 1, "terminating-regression"),
                ("Write-Error nonterminating-regression", 1, "nonterminating-regression"),
                ("cmd.exe /D /C exit 7", 7, ""),
                ("Get-Item missing-native-shell-regression-file", 1, "missing-native-shell-regression-file"),
                ("Write-Output recovered # trailing comment", 0, "recovered"),
                ("cmd.exe /D /C exit 9", 9, ""),
                ("Write-Output fresh-status", 0, "fresh-status"),
                ("Write-Output pipeline | ForEach-Object { Write-Error pipeline-regression }", 1, "pipeline-regression"),
                ("Get-Item missing-native-shell-regression-file; Write-Output final-success", 0, "final-success"),
                ("$env:GITCODE_NATIVE_REGRESSION='retained'; Write-Output $env:GITCODE_NATIVE_REGRESSION", 0, "retained"),
                ("Write-Output $env:GITCODE_NATIVE_REGRESSION", 0, "retained"),
                ("[pscustomobject]@{ Name='formatted-regression'; Value=42 }", 0, "formatted-regression"),
                ("Write-Output clean-after-format", 0, "clean-after-format"),
            ] {
                let output = session.execute(&engine, command, Duration::from_secs(15)).await.unwrap();
                assert_eq!(output.exit_code, Some(expected), "{selection}: {command}: {}", output.output);
                assert!(output.output.contains(text), "{selection}: {command}: {}", output.output);
                if command == "Write-Output clean-after-format" {
                    assert_eq!(output.output.trim(), "clean-after-format", "{selection}");
                }
            }
        }
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
        let job = session
            .background(&engine, slow, Duration::from_secs(30))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        job.kill().await.unwrap();
        let output = session
            .execute(&engine, "echo after-cancel", Duration::from_secs(15))
            .await
            .unwrap();
        assert_eq!(output.exit_code, Some(0), "{selection}: {}", output.output);
        assert!(output.output.contains("after-cancel"), "{selection}");
        session.close().await;
    }
    std::fs::remove_dir_all(root).unwrap();
}
