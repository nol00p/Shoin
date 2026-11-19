//! The words a setting takes, written once.
//!
//! `:set`, the commands that take a value, Tab completion, the config
//! checks, `--format` and `:help` all read these lists, so a value added to a
//! parser is added here and every message and completion names it.

/// Every `:set` option: its name, and the other names it answers to.
/// `set_option` reads keys through this table and Tab completes the first
/// column.
pub(crate) const OPTIONS: &[(&str, &[&str])] = &[
    ("align", &[]),
    ("autoreload", &[]),
    ("autosave", &[]),
    ("autosave_interval", &["autosave_min"]),
    ("clipboard", &["cb"]),
    ("code_syntax", &["syntax"]),
    ("conceal", &[]),
    ("embed", &[]),
    ("focus", &[]),
    ("hints", &[]),
    ("line_spacing", &["spacing"]),
    ("measure", &["width"]),
    ("mouse", &[]),
    ("numbers", &["number", "nu"]),
    ("tab_width", &["ts"]),
    ("typewriter", &["tw"]),
];

/// The words that turn a toggle option on; any other value turns it off.
pub(crate) const ON_WORDS: &[&str] = &["on", "true", "1", "yes"];

/// What `:set` offers for a toggle option.
pub(crate) const TOGGLE_VALUES: &[&str] = &["off", "on"];

/// The values `:focus` and `:set focus` take, as `FocusMode::parse` reads
/// them and its labels name them.
pub(crate) const FOCUS_VALUES: &[&str] = &["off", "paragraph", "sentence"];

/// The values `:align` and `:set align` take (`TextAlign::parse`).
pub(crate) const ALIGN_VALUES: &[&str] = &["left", "center", "right", "justified"];

/// The values `:embed` and `:set embed` take (`transclude::Mode::parse`).
pub(crate) const EMBED_VALUES: &[&str] = &["none", "short", "rec", "full"];

/// The values `:number` and `:set numbers` take (`NumberMode::parse`).
pub(crate) const NUMBER_VALUES: &[&str] = &["off", "relative", "absolute"];

/// The formats `:export` takes (`export::Format::parse`).
pub(crate) const EXPORT_VALUES: &[&str] = &["md", "txt", "html", "pdf"];

/// The words that ask `:export` to justify, in either place beside the
/// format. The first is the one Tab offers.
pub(crate) const JUSTIFY_WORDS: &[&str] = &["justify", "justified"];

/// Whether `:export` can justify `format`: plain text and HTML only.
pub(crate) fn export_justifies(format: crate::export::Format) -> bool {
    use crate::export::Format;
    matches!(format, Format::Html | Format::Text)
}

/// `values` as a sentence's list: "md, txt, html or pdf".
pub(crate) fn or_list<S: AsRef<str>>(values: &[S]) -> String {
    match values {
        [] => String::new(),
        [one] => one.as_ref().to_string(),
        [rest @ .., last] => {
            let rest: Vec<&str> = rest.iter().map(AsRef::as_ref).collect();
            format!("{} or {}", rest.join(", "), last.as_ref())
        }
    }
}

/// `values` as a config check or a flash names them: "off · paragraph · sentence".
pub(crate) fn dot_list(values: &[&str]) -> String {
    values.join(" · ")
}

/// The `:export` formats that take `justify`, in `EXPORT_VALUES` order.
pub(crate) fn justify_formats() -> Vec<&'static str> {
    EXPORT_VALUES
        .iter()
        .copied()
        .filter(|v| crate::export::Format::parse(v).is_some_and(export_justifies))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_read_as_words() {
        assert_eq!(or_list::<&str>(&[]), "");
        assert_eq!(or_list(&["md"]), "md");
        assert_eq!(or_list(&["md", "txt"]), "md or txt");
        assert_eq!(or_list(EXPORT_VALUES), "md, txt, html or pdf");
        assert_eq!(dot_list(FOCUS_VALUES), "off · paragraph · sentence");
        assert_eq!(justify_formats(), ["txt", "html"]);
    }
}
