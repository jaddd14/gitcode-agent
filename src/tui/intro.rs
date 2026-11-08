//! Monotonic, bounded startup animation with immediate event cancellation.

use super::header::mascot;
use crate::theme::{foreground, ColorMode, Theme};
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures_util::StreamExt;
use std::io;
use std::time::{Duration, Instant};

/// Total intro duration; any key or paste skips it without an additional delay.
pub const DURATION: Duration = Duration::from_millis(700);
/// Result preserving an early event for application dispatch.
#[derive(Debug)]
pub enum IntroEnd {
    /// Animation reached its monotonic deadline.
    Finished,
    /// Input was received and must be forwarded to the editor, not discarded.
    Skipped(Event),
    /// Terminal event source closed.
    Closed,
}
/// Deterministic animation frame; only the theme's flat colors are used.
pub fn frame(elapsed: Duration, width: usize, height: usize, mode: ColorMode) -> Vec<String> {
    let rows = height.clamp(1, 6);
    let width = width.max(1);
    let theme = Theme::default();
    let progress = (elapsed.as_millis().min(700) * 16 / 700) as usize;
    let sprite = mascot(&theme, mode);
    if progress >= 16 {
        if width < 16 {
            return mascot(&theme, ColorMode::None)
                .into_iter()
                .take(rows)
                .map(|s| super::clip(&s, width))
                .collect();
        }
        return sprite.into_iter().take(rows).collect();
    }
    let tick = elapsed.as_millis() as usize / 35;
    (0..rows)
        .map(|row| {
            let mut line = String::new();
            for col in 0..width.min(40) {
                let petal = (col + row * 7 + tick).is_multiple_of(17);
                if petal {
                    line.push_str(&foreground(
                        if row % 2 == 0 {
                            theme.pink
                        } else {
                            theme.purple_soft
                        },
                        mode,
                    ));
                    line.push('·');
                } else if col < progress && col < 16 {
                    let top = super::header::SAKURA[row * 2].as_bytes()[col];
                    let bottom = super::header::SAKURA[row * 2 + 1].as_bytes()[col];
                    if top != b' ' || bottom != b' ' {
                        line.push_str(&foreground(theme.pink, mode));
                        line.push('▀');
                    } else {
                        line.push(' ');
                    }
                } else {
                    line.push(' ');
                }
            }
            if mode != ColorMode::None {
                line.push_str("\x1b[0m");
            }
            line
        })
        .collect()
}
/// Animate on absolute deadlines; the callback paints frames and no network work runs.
///
/// Keep using the same EventStream after this returns; concurrent terminal readers
/// are unsupported by crossterm and would race for the first keystroke.
pub async fn play(
    events: &mut EventStream,
    mut paint: impl FnMut(Duration) -> io::Result<()>,
) -> io::Result<IntroEnd> {
    let start = Instant::now();
    let end = start + DURATION;
    let mut next = start;
    loop {
        tokio::select! {
            biased;
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Release => {},
                Some(Ok(event @ (Event::Key(_) | Event::Paste(_)))) => return Ok(IntroEnd::Skipped(event)),
                Some(Ok(_)) => {},
                Some(Err(error)) => return Err(error),
                None => return Ok(IntroEnd::Closed),
            },
            _ = tokio::time::sleep_until(next.into()) => {
                let now = Instant::now();
                paint(now.saturating_duration_since(start).min(DURATION))?;
                if now >= end { return Ok(IntroEnd::Finished); }
                let frame = now.duration_since(start).as_millis() / 35 + 1;
                next = (start + Duration::from_millis(frame as u64 * 35)).min(end);
            }
        }
    }
}
