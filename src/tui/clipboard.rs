//! Explicit user-requested clipboard operations; never interpolate user data as shell code.

use std::io::{self, IsTerminal};

use crate::provider::canonical::Content;
#[cfg(any(windows, test))]
use base64::{engine::general_purpose::STANDARD, Engine};

#[cfg(any(windows, test))]
const MAX_PNG_BYTES: usize = 4 * 1024 * 1024;
#[cfg(any(windows, test))]
const MAX_BASE64_BYTES: usize = MAX_PNG_BYTES.div_ceil(3) * 4;

/// Read only after an explicit user key action, and never in a redirected session.
pub async fn paste_image() -> io::Result<Option<Content>> {
    paste_image_if_interactive(io::stdin().is_terminal() && io::stdout().is_terminal()).await
}

async fn paste_image_if_interactive(interactive: bool) -> io::Result<Option<Content>> {
    if !interactive {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "interactive clipboard required",
        ));
    }
    read_native_image().await
}

// Validate the fixed script's bounded transport, base64, PNG signature and IHDR dimensions.
// The PNG encoder is Windows.Forms/System.Drawing, not clipboard-provided executable code.
#[cfg(any(windows, test))]
fn image_from_output(output: &[u8]) -> io::Result<Option<Content>> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid or oversized clipboard PNG",
        )
    };
    if output.len() > MAX_BASE64_BYTES {
        return Err(invalid());
    }
    if output.is_empty() {
        return Ok(None);
    }
    let png = STANDARD.decode(output).map_err(|_| invalid())?;
    if png.len() > MAX_PNG_BYTES
        || png.len() < 33
        || &png[..8] != b"\x89PNG\r\n\x1a\n"
        || &png[8..16] != b"\0\0\0\rIHDR"
    {
        return Err(invalid());
    }
    let width = u32::from_be_bytes(png[16..20].try_into().map_err(|_| invalid())?);
    let height = u32::from_be_bytes(png[20..24].try_into().map_err(|_| invalid())?);
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 16_000_000 {
        return Err(invalid());
    }
    Ok(Some(Content::Image {
        media_type: "image/png".into(),
        data: STANDARD.encode(png),
    }))
}

#[cfg(windows)]
async fn read_native_image() -> io::Result<Option<Content>> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;
    // Entire script is fixed. Clipboard pixels are encoded as PNG, then base64;
    // no clipboard text, file path, or user input is ever evaluated as PowerShell.
    const SCRIPT: &str = r#"$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; $image=[System.Windows.Forms.Clipboard]::GetImage(); if ($null -eq $image) { exit 0 }; try { if ([long]$image.Width * [long]$image.Height -gt 16000000) { throw 'Image too large' }; $stream=New-Object System.IO.MemoryStream; try { $image.Save($stream,[System.Drawing.Imaging.ImageFormat]::Png); if ($stream.Length -gt 4194304) { throw 'Image too large' }; [Console]::Out.Write([Convert]::ToBase64String($stream.ToArray())) } finally { $stream.Dispose() } } finally { $image.Dispose() }"#;
    let shell = crate::shell::find_executable("powershell.exe")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "PowerShell unavailable"))?;
    let mut child = tokio::process::Command::new(shell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-STA",
            "-Command",
            SCRIPT,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("clipboard stdout unavailable"))?;
        let mut output = Vec::new();
        stdout
            .take((MAX_BASE64_BYTES + 1) as u64)
            .read_to_end(&mut output)
            .await?;
        if output.len() > MAX_BASE64_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "clipboard image too large",
            ));
        }
        if !child.wait().await?.success() {
            return Err(io::Error::other(
                "clipboard image read failed or image too large",
            ));
        }
        image_from_output(&output)
    })
    .await;
    result.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "clipboard image read timed out"))?
}

#[cfg(not(windows))]
async fn read_native_image() -> io::Result<Option<Content>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "clipboard image paste is supported only on Windows",
    ))
}

/// Write text only from an interactive terminal, never from a redirected/test command.
pub async fn copy_text(text: &str) -> io::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "interactive clipboard required",
        ));
    }
    copy_native(text).await
}

#[cfg(windows)]
async fn copy_native(text: &str) -> io::Result<()> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    // Set-Clipboard is shipped with Windows PowerShell 5.1. UTF-8 stdin carries
    // only data; no response content is interpolated into this fixed script.
    let shell = crate::shell::find_executable("powershell.exe")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "PowerShell unavailable"))?;
    let mut child = tokio::process::Command::new(shell)
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-STA", "-Command",
            "$ErrorActionPreference='Stop'; [Console]::InputEncoding = New-Object System.Text.UTF8Encoding; $text = [Console]::In.ReadToEnd(); Set-Clipboard -Value $text"])
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null())
        .kill_on_drop(true).spawn()?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("clipboard stdin unavailable"))?;
        input.write_all(text.as_bytes()).await?;
        input.shutdown().await?;
        drop(input);
        if child.wait().await?.success() {
            Ok(())
        } else {
            Err(io::Error::other("clipboard write failed"))
        }
    })
    .await;
    result.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "clipboard write timed out"))?
}

#[cfg(not(windows))]
async fn copy_native(_text: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native clipboard unavailable",
    ))
}

#[cfg(test)]
mod image_tests {
    use super::*;

    #[tokio::test]
    async fn redirected_sessions_never_access_native_clipboard() {
        assert_eq!(
            paste_image_if_interactive(false)
                .await
                .err()
                .map(|error| error.kind()),
            Some(io::ErrorKind::Unsupported)
        );
    }

    #[test]
    fn injected_output_requires_bounded_base64_png_and_dimensions(
    ) -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(image_from_output(b"")?, None);
        for output in [b"not base64!".as_slice(), b"aGVsbG8=".as_slice()] {
            assert!(image_from_output(output).is_err());
        }
        assert!(image_from_output(&vec![b'A'; MAX_BASE64_BYTES + 1]).is_err());
        // A real, fixed 1x1 PNG fixture; validation never accesses the OS clipboard.
        let png = b"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aX2kAAAAASUVORK5CYII=";
        assert!(
            matches!(image_from_output(png)?, Some(Content::Image { media_type, data }) if media_type == "image/png" && data.as_bytes() == png)
        );
        let mut decoded = STANDARD.decode(png)?;
        decoded[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(image_from_output(STANDARD.encode(decoded).as_bytes()).is_err());
        Ok(())
    }
}
