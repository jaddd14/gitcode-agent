//! Interpreter metadata and native executable lookup.

pub use crate::platform::{Shell, ShellKind};

impl Shell {
    /// Discover an explicitly selected interpreter or the native default.
    pub fn detect(selection: &str) -> Result<Self, String> {
        crate::platform::detect_shell(selection)
    }

    /// Render a state-preserving command followed by an unambiguous status marker.
    pub fn script(&self, command: &str, id: u64) -> String {
        self.script_with_marker(command, &format!("M7{id}"))
    }

    pub(crate) fn script_with_marker(&self, command: &str, marker: &str) -> String {
        match self.kind {
            ShellKind::Bash => format!(
                "{{ {command}\n}} </dev/null\nbuiltin printf '\\036{marker}:%s\\037' \"$?\"\n"
            ),
            ShellKind::Pwsh | ShellKind::PowerShell => {
                // Capture status in the user's script block: invoking a script block
                // can itself succeed after a nonterminating error inside it. A newline
                // also keeps a trailing user comment from swallowing the capture.
                let body = format!(
                    "{command}\n$__gc_ok=$?; if ($__gc_ok) {{ $__gc_code=0 }} elseif ($LASTEXITCODE) {{ $__gc_code=$LASTEXITCODE }} else {{ $__gc_code=1 }}"
                );
                use base64::Engine;
                // Keep the stdin protocol one physical line. PowerShell's
                // -Command - parser otherwise buffers multiline quoted input.
                let encoded = base64::engine::general_purpose::STANDARD.encode(body.as_bytes());
                // Dot-sourcing retains cwd/env; drain object formatting before framing.
                // Reset native status per request so an earlier failure cannot leak.
                format!("[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $global:LASTEXITCODE=0; $__gc_code=1; . {{ try {{ . ([scriptblock]::Create([System.Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{encoded}')))) }} catch {{ Write-Output $_; $__gc_code=1 }} }} | Out-Default; [Console]::Out.Write(([char]30)+'{marker}:'+$__gc_code+([char]31)); [Console]::Out.Flush()\r\n")
            }
            ShellKind::Cmd => {
                format!("@chcp 65001 >nul\r\n{command}\r\n@echo \x1e{marker}:%errorlevel%\x1f\r\n")
            }
        }
    }
}

/// Find an executable, including Windows PATHEXT shims. Never uses the current directory implicitly.
pub fn find_executable(program: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(program);
    let extensions: Vec<std::ffi::OsString> = {
        #[cfg(windows)]
        {
            std::env::var_os("PATHEXT")
                .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into())
                .to_string_lossy()
                .split(';')
                .map(Into::into)
                .collect()
        }
        #[cfg(not(windows))]
        {
            Vec::new()
        }
    };
    let candidates = if path.is_absolute() || path.components().count() > 1 {
        vec![path.to_path_buf()]
    } else {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|value| {
                std::env::split_paths(&value)
                    .filter(|p| p.is_absolute())
                    .map(|p| p.join(path))
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    for candidate in candidates {
        if candidate.is_file() {
            return Some(candidate);
        }
        if candidate.extension().is_none() {
            for extension in &extensions {
                let mut name = candidate.as_os_str().to_os_string();
                name.push(extension);
                let path = std::path::PathBuf::from(name);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}
