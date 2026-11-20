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
    check(r"x^{\frac{1}{2}}", &[" 1", " ─", " 2", "x "]);
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
    ] {
        println!("{tex}");
        for row in layout(tex, 80).unwrap() {
            println!("{row}");
        }
        println!();
    }
}
