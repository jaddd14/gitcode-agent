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
