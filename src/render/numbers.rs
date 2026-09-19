//! Line-number gutter mode.
//!
//! `off` draws no gutter. `absolute` numbers every line from the top, 1-based.
//! `relative` numbers by distance from the cursor's line, which reads `0`.
//! Vim's `number` and `relativenumber` folded into one setting rather than two
//! independent booleans — nothing here needs their hybrid ("number +
//! relativenumber", current line absolute and every other relative) third
//! state, so one mode covers what a reader actually reaches for.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NumberMode {
    #[default]
    Off,
    Relative,
    Absolute,
}

impl NumberMode {
    /// `None` for a word this does not know, so a typo in `layout.numbers` is
    /// reported rather than silently read as `off`.
    pub fn parse(s: &str) -> Option<NumberMode> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "false" => NumberMode::Off,
            "relative" | "rel" | "rnu" => NumberMode::Relative,
            "absolute" | "abs" | "nu" | "number" | "on" | "true" => NumberMode::Absolute,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            NumberMode::Off => "off",
            NumberMode::Relative => "relative",
            NumberMode::Absolute => "absolute",
        }
    }

    pub fn is_on(self) -> bool {
        !matches!(self, NumberMode::Off)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_the_documented_words_and_rejects_the_rest() {
        assert_eq!(NumberMode::parse("off"), Some(NumberMode::Off));
        assert_eq!(NumberMode::parse("RELATIVE"), Some(NumberMode::Relative));
        assert_eq!(NumberMode::parse(" absolute "), Some(NumberMode::Absolute));
        assert_eq!(NumberMode::parse("rnu"), Some(NumberMode::Relative));
        assert_eq!(NumberMode::parse("nu"), Some(NumberMode::Absolute));
        assert_eq!(NumberMode::parse("banana"), None);
    }

    #[test]
    fn only_off_is_off() {
        assert!(!NumberMode::Off.is_on());
        assert!(NumberMode::Relative.is_on());
        assert!(NumberMode::Absolute.is_on());
    }
}
