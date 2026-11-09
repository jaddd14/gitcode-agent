//! Small compile-time UI string table.

/// Supported interface locale.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Language {
    /// Russian, the default interface language.
    #[default]
    Ru,
    /// English.
    En,
}

/// Stable string identifiers, independent of visible labels.
#[derive(Debug, Clone, Copy)]
pub enum Key {
    /// Input readiness label.
    Ready,
    /// Cancellation label.
    Cancelled,
    /// Permission approval action.
    Approve,
    /// Permission rejection action.
    Deny,
    /// Context usage label.
    Context,
    /// Staged image indicator in the prompt.
    ImageAttached,
    /// Successful explicit clipboard image staging.
    ImageStaged,
    /// Clipboard did not contain an image.
    ClipboardNoImage,
    /// Prefix for a failed explicit image read.
    ImageNotAttached,
    /// Slash commands cannot carry image attachments.
    CommandImageRejected,
    /// Privacy-safe completion notification.
    TurnCompleted,
}

/// Resolve an interface label without allocating.
pub fn text(language: Language, key: Key) -> &'static str {
    match (language, key) {
        (Language::Ru, Key::Ready) => "Готов",
        (Language::Ru, Key::Cancelled) => "Отменено",
        (Language::Ru, Key::Approve) => "Разрешить",
        (Language::Ru, Key::Deny) => "Отклонить",
        (Language::Ru, Key::Context) => "Контекст",
        (Language::En, Key::Ready) => "Ready",
        (Language::En, Key::Cancelled) => "Cancelled",
        (Language::En, Key::Approve) => "Approve",
        (Language::En, Key::Deny) => "Deny",
        (Language::En, Key::Context) => "Context",
        (Language::Ru, Key::ImageAttached) => {
            "[PNG прикреплён · Enter — отправить · Esc/Ctrl+C — убрать]"
        }
        (Language::En, Key::ImageAttached) => "[PNG attached · Enter sends · Esc/Ctrl+C clears]",
        (Language::Ru, Key::ImageStaged) => {
            "PNG прикреплён к черновику; Enter — отправить, Esc/Ctrl+C — убрать"
        }
        (Language::En, Key::ImageStaged) => {
            "PNG attached to this draft; Enter sends, Esc/Ctrl+C clears"
        }
        (Language::Ru, Key::ClipboardNoImage) => {
            "В буфере обмена нет изображения; черновик не изменён"
        }
        (Language::En, Key::ClipboardNoImage) => {
            "Clipboard contains no image; existing draft unchanged"
        }
        (Language::Ru, Key::ImageNotAttached) => "Изображение не прикреплено",
        (Language::En, Key::ImageNotAttached) => "Image not attached",
        (Language::Ru, Key::CommandImageRejected) => {
            "Команда не выполнена; изображение убрано. Вставьте его снова с обычным запросом"
        }
        (Language::En, Key::CommandImageRejected) => {
            "Command not run; image discarded. Paste it again with a normal prompt"
        }
        (Language::Ru, Key::TurnCompleted) => "Ход завершён",
        (Language::En, Key::TurnCompleted) => "Turn completed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn russian_is_default() {
        assert_eq!(text(Language::default(), Key::Ready), "Готов");
        assert_eq!(text(Language::En, Key::Ready), "Ready");
    }
}
