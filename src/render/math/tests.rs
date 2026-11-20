use super::layout;
use crate::render::layout::display_width;

fn check(tex: &str, expected: &[&str]) {
    let width = usize::from(display_width(expected[0]));
    let actual = layout(tex, width).unwrap_or_else(|| panic!("could not lay out {tex:?} at width {width}; at 80: {:?}", layout(tex, 80)));
    assert_eq!(actual, expected, "{tex}");
    assert!(actual.iter().all(|row| usize::from(display_width(row)) == width));
    assert_eq!(layout(tex, width - 1), None, "{tex}");
}

#[test]
fn fractions() {
    check(r"x = \frac{1}{ab}", &["    1 ", "x = ──", "    ab"]);
    check(r"\dfrac{\tfrac{1}{2}}{abc}", &[" 1 ", " ─ ", " 2 ", "───", "abc"]);
    check(r"\frac a b", &["a", "─", "b"]);
}

#[test]
fn scripts_and_limits() {
    check(r"x^2 a_i x^{10}", &["x²aᵢx¹⁰"]);
    check(r"F_z", &["F ", " z"]);
    check(r"x^{a+b}_{z}", &[" a+b", "x   ", "  z "]);
    check(r"x^{z+1}", &[" z+1", "x   "]);
    check(r"\sum_{i=1}^n", &[" n ", " ∑ ", "i=1"]);
    check(r"\int_0^1", &[" ¹", "∫ ", " ₀"]);
    check(r"\lim_{x\to0} x", &["lim x", "x→0  "]);
    for (command, glyph) in [
        ("prod", "∏"), ("coprod", "∐"), ("bigcup", "⋃"), ("bigcap", "⋂"),
        ("bigoplus", "⨁"), ("bigotimes", "⨂"),
    ] {
        check(&format!(r"\{command}_i^n"), &["n", glyph, "i"]);
    }
}

#[test]
fn superscript_sits_above_base_baseline() {
    check(r"x^{\frac{1}{2}}", &[" 1", " ─", " 2", "x "]);
}

#[test]
fn subscript_sits_below_base_baseline() {
    check(r"x_{\frac{1}{2}}", &["x ", " 1", " ─", " 2"]);
}

#[test]
fn paired_scripts_share_a_column() {
    check(r"x^{\frac{1}{2}}_z", &[" 1", " ─", " 2", "x ", " z"]);
    check(r"x_{\frac{a}{b}}^{\frac{c}{d}}", &[" c", " ─", " d", "x ", " a", " ─", " b"]);
}

#[test]
fn script_with_empty_first_row() {
    check(r"x^{\begin{gather}\\̂\end{gather}}", &[" ", " ̂", "x"]);
}

#[test]
fn zero_width_parts_join_only_occupied_rows() {
    let count = 12_000;
    let tex = format!("{}\\begin{{gather}}{}x\\end{{gather}}", "̂".repeat(count), "x\\\\".repeat(count));
    let start = std::time::Instant::now();
    let rows = layout(&tex, 1).unwrap();
    assert!(start.elapsed() < std::time::Duration::from_secs(1), "{:?}", start.elapsed());
    assert_eq!(rows.len(), count + 1);
    assert_eq!(rows[count / 2], format!("{}x", "̂".repeat(count)));
}

#[test]
fn accent_skips_baseline_padding() {
    check(r"\hat{\sum_{i=1}}", &[" ∑̂ ", "i=1"]);
}

#[test]
fn explicit_negative_space_suppresses_operator_spacing() {
    check(r"\sin\!x", &["sinx"]);
    check(r"\sin x", &["sin x"]);
    check(r"x\!\sin\!\theta", &["xsinθ"]);
}

#[test]
fn column_separators_require_a_column_environment() {
    assert_eq!(layout("a&b", 80), None);
    assert_eq!(layout(r"\begin{gather}a&b\end{gather}", 80), None);
    check(r"a\&b", &["a&b"]);
}

#[test]
fn fallback_requires_paired_delimiters() {
    assert_eq!(layout(r"\begin{custom}\left(x\end{custom}", 80), None);
    assert_eq!(layout(r"\begin{custom}x\right)\end{custom}", 80), None);
    assert_eq!(layout(r"\begin{custom}{\left(x}\right)\end{custom}", 80), None);
    assert_eq!(layout(r"\begin{custom}\left(\begin{inner}x\right)\end{inner}\end{custom}", 80), None);
    check(r"\begin{custom}\left(\left[x\right]\right)\end{custom}", &["([x])"]);
}

#[test]
fn matrices_and_delimiters() {
    for (name, expected) in [
        ("matrix", ["a  b", "c  d"]),
        ("pmatrix", ["⎛a  b⎞", "⎝c  d⎠"]),
        ("bmatrix", ["⎡a  b⎤", "⎣c  d⎦"]),
        ("Bmatrix", ["⎧a  b⎫", "⎩c  d⎭"]),
        ("vmatrix", ["│a  b│", "│c  d│"]),
        ("Vmatrix", ["‖a  b‖", "‖c  d‖"]),
    ] {
        check(&format!(r"\begin{{{name}}}a&b\\c&d\end{{{name}}}"), &expected);
    }
    check(r"\begin{pmatrix}a&b\end{pmatrix}", &["(a  b)"]);
    check(r"\left(\frac{1}{2}\right)", &["⎛1⎞", "⎜─⎟", "⎝2⎠"]);
    check(r"\left.\frac{1}{2}\right\}", &["1⎫", "─⎬", "2⎭"]);
    check(r"\left\{\begin{gather}a\\b\\c\\d\\e\end{gather}\right.",
        &["⎧a", "⎪b", "⎨c", "⎪d", "⎩e"]);
    check(r"\begin{matrix}猫&x\\a&\frac{b}{c}\end{matrix}",
        &["猫  x", "    b", "a   ─", "    c"]);
}

#[test]
fn roots() {
    check(r"\sqrt{x}", &[" ‾", "√x"]);
    check(r"\sqrt[3]{x}", &["  ‾", "³√x"]);
    check(r"\sqrt[z]{x}", &["z ‾", " √x"]);
    check(r"\sqrt{\frac{\frac{1}{2}}{x+1}}",
        &[" ‾‾‾‾‾", "   1  ", "   ─  ", "   2  ", "√─────", " x + 1"]);
}

#[test]
fn equations() {
    check(r"\begin{cases}x^2 & \text{if } x>0 \\ 0 & \text{otherwise}\end{cases}",
        &["⎧x²  if x > 0 ", "⎩0   otherwise"]);
    for name in ["aligned", "align", "align*"] {
        check(&format!(r"\begin{{{name}}}a &= b \\ abc &= d+1\end{{{name}}}"),
            &["  a = b    ", "abc = d + 1"]);
    }
    check(r"x=1\\y=123", &[" x = 1 ", "y = 123"]);
    check(r"\begin{gather}x=1\\y=123\\\end{gather}", &[" x = 1 ", "y = 123"]);
}

#[test]
fn shared_symbols_and_fallbacks() {
    check(r"\foo + \alpha", &[r"\foo + α"]);
    check(r"\begin{custom}\frac{1}{2} & \alpha \\ F_z\end{custom}",
        &["1⁄2    α  │  F_z"]);
    // Inline fallback is selected before checking the display width of the body.
    check(r"\begin{custom}a+b\end{custom}", &["a+b"]);
    check(r"\hat{猫} + \vec{x}\,\mathrm{kg}\;\mathbf{θ}\quad\sin\left(x\right)\qquad\hat{i}",
        &["猫̂ + x⃗ kg  θ  sin(x)    î"]);
    check(r"-x-\theta\in A", &["−x − θ ∈ A"]);
}

#[test]
fn malformed_and_empty_input() {
    for tex in ["", " \t\n", "{}", r"\quad", r"\frac{2}{", "x}", "x^", "x^1^2",
        r"\begin{matrix}x", r"\begin{matrix}x\end{cases}",
        r"\begin{custom}\begin{foo}x\end{custom}",
        r"\begin{custom}{x\end{custom}", r"\left(x", r"\right)", r"\sqrt[3{x}",
    ] {
        assert_eq!(layout(tex, 80), None, "{tex:?}");
    }
    assert_eq!(layout("猫", 1), None);
    assert_eq!(layout("x", 0), None);
    assert_eq!(layout(&"{".repeat(2048), 80), None);
    assert_eq!(layout(&"x".repeat(65_537), 65_536), None);
}

#[test]
fn issue_example() {
    check(r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2}",
        &[" n     n(n + 1)", " ∑ i = ────────", "i=1       2    "]);
}

#[test]
#[ignore = "exercise randomly assembled TeX with cargo test --release -- --ignored fuzz"]
fn fuzz_display_math() {
    let tokens = [
        "x", "猫", " ", "\n", "̂", "+", "-", "=", "0", "{", "}", "[", "]", "_", "^", "&",
        r"\\", r"\frac", r"\sqrt", r"\sum", r"\left(", r"\right)", r"\begin{matrix}",
        r"\end{matrix}", r"\begin{custom}", r"\end{custom}", r"\foo", r"\alpha",
        r"\text{if }", r"\hat{犬}", r"x^2", r"\frac{a}{b}", r"\begin{pmatrix}w&z\end{pmatrix}",
    ];
    let mut state = 0x4c41_594f_5554u64;
    let mut random = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state as usize
    };
    for _ in 0..10_000 {
        let width = random() % 100;
        let mut tex = String::new();
        for _ in 0..random() % 40 {
            tex.push_str(tokens[random() % tokens.len()]);
        }
        if let Some(rows) = layout(&tex, width) {
            let actual = usize::from(display_width(&rows[0]));
            assert!(actual <= width && actual > 0, "{tex:?} {rows:?}");
            assert!(rows.iter().all(|row| !row.contains(['\r', '\n', '\t'])
                && usize::from(display_width(row)) == actual), "{tex:?} {rows:?}");
        }
    }
}

#[test]
#[ignore = "print representative display formulas for terminal inspection"]
fn display_math_examples() {
    for tex in [
        r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2}",
        r"\begin{pmatrix}a&b\\c&d\end{pmatrix}",
        r"\begin{cases}x^2 & \text{if } x>0 \\ 0 & \text{otherwise}\end{cases}",
        r"\begin{aligned}a &= b \\ abc &= d+1\end{aligned}",
        r"\sqrt{\frac{\frac{1}{2}}{x+1}}",
        r"x^{\frac{1}{2}}",
        r"x_{\frac{1}{2}}",
        r"x_{\frac{a}{b}}^{\frac{c}{d}}",
        r"x^{\begin{gather}\\̂\end{gather}}",
        r"\hat{\sum_{i=1}}",
        r"\sin\!x",
        r"a\&b",
    ] {
        println!("{tex}");
        for row in layout(tex, 80).unwrap() {
            println!("{row}");
        }
        println!();
    }
}
