//! Interpreter metadata and native executable lookup.

pub use crate::platform::{Shell, ShellKind};

impl Shell {
    /// Discover an explicitly selected interpreter or the native default.
    pub fn detect(selection: &str) -> Result<Self, String> {
        crate::platform::detect_shell(selection)
    }

    /// Render a state-preserving command followed by an unambiguous status marker.
    pub fn script(&self, command: &str, id: u64) -> String {
        match self.kind {
            ShellKind::Bash => format!(
                "{{ {command}\n}} </dev/null\nbuiltin printf '\\036M7{id}:%s\\037' \"$?\"\n"
            ),
            ShellKind::Pwsh | ShellKind::PowerShell => {
                let escaped = command.replace('\'', "''");
                // Drain PowerShell's object formatter before writing raw framing bytes.
                // Capture status inside the dot-sourced block, before Out-Default
                // can replace `$?`; dot-sourcing preserves cwd and environment.
                format!("[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $global:LASTEXITCODE=0; . {{ try {{ . ([scriptblock]::Create('{escaped}')); $__gc_ok=$?; if ($__gc_ok) {{ $__gc_code=0 }} elseif ($LASTEXITCODE) {{ $__gc_code=$LASTEXITCODE }} else {{ $__gc_code=1 }} }} catch {{ Write-Output $_; $__gc_code=1 }} }} | Out-Default; [Console]::Out.Write(([char]30)+'M7{id}:'+$__gc_code+([char]31)); [Console]::Out.Flush()\r\n")
            }
            ShellKind::Cmd => {
                format!("@chcp 65001 >nul\r\n{command}\r\n@echo \x1eM7{id}:%errorlevel%\x1f\r\n")
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
