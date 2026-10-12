//! Message language. English by default; Ukrainian when the locale is `uk*` (or `USTAN_LANG=uk`).
//! Messages are written with [`t!`]: the Ukrainian and the English text side by side.
use std::sync::OnceLock;

pub fn english() -> bool {
    static EN: OnceLock<bool> = OnceLock::new();
    *EN.get_or_init(|| {
        for var in ["USTAN_LANG", "LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"] {
            if let Ok(v) = std::env::var(var) {
                if !v.is_empty() {
                    return !v.to_lowercase().starts_with("uk");
                }
            }
        }
        true
    })
}

/// Text of a cancelled operation, as errors display it.
pub fn cancelled() -> &'static str {
    if english() { "cancelled" } else { "скасовано" }
}

/// `t!("українською {x}", "in English {x}", args…)`: a `format!` in the user's language.
#[macro_export]
macro_rules! t {
    ($uk:literal, $en:literal $(, $($arg:tt)*)?) => {
        if $crate::i18n::english() { format!($en $(, $($arg)*)?) } else { format!($uk $(, $($arg)*)?) }
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn t_formats_in_the_chosen_language_and_keeps_captures() {
        let n = 3;
        let s = t!("є {n} шт. {}", "{n} items {}", "x");
        assert!(s == "є 3 шт. x" || s == "3 items x", "{s}");
        assert_eq!(t!("просто", "plain"), if super::english() { "plain" } else { "просто" });
    }
}
