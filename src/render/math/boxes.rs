use super::symbols::{subscript_char, superscript_char};
use crate::render::layout::cell_width;

pub(super) struct MathBox {
    pub width: usize,
    pub baseline: usize,
    pub rows: Vec<String>,
}

#[derive(Clone, Copy)]
pub(super) enum Align {
    Left,
    Center,
    Right,
}

impl MathBox {
    pub fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self { width: text.chars().map(|c| cell_width(c, 1)).sum(), baseline: 0, rows: vec![text] }
    }

    fn descent(&self) -> usize {
        self.rows.len() - self.baseline - 1
    }

    pub fn join(parts: Vec<Self>, limit: usize) -> Option<Self> {
        let width = parts.iter().map(|part| part.width).sum();
        if width > limit {
            return None;
        }
        let baseline = parts.iter().map(|part| part.baseline).max().unwrap_or(0);
        let descent = parts.iter().map(Self::descent).max().unwrap_or(0);
        let mut rows = vec![String::new(); baseline + descent + 1];
        for part in parts {
            let start = baseline - part.baseline;
            let part_height = part.rows.len();
            let (before, rest) = rows.split_at_mut(start);
            for (row, content) in rest[..part_height].iter_mut().zip(part.rows) {
                row.push_str(&content);
            }
            if part.width > 0 {
                let padding = " ".repeat(part.width);
                for row in before.iter_mut().chain(&mut rest[part_height..]) {
                    row.push_str(&padding);
                }
            }
        }
        Some(Self { width, baseline, rows })
    }

    pub fn pad(mut self, width: usize, align: Align) -> Self {
        let extra = width - self.width;
        let left = match align {
            Align::Left => 0,
            Align::Center => extra / 2,
            Align::Right => extra,
        };
        let prefix = " ".repeat(left);
        let suffix = " ".repeat(extra - left);
        for row in &mut self.rows {
            *row = format!("{prefix}{row}{suffix}");
        }
        self.width = width;
        self
    }

    pub fn stack(above: Option<Self>, center: Self, below: Option<Self>) -> Self {
        let width = [&above, &below].into_iter().flatten()
            .map(|part| part.width).fold(center.width, usize::max);
        let mut rows = Vec::new();
        if let Some(top) = above {
            rows.extend(top.pad(width, Align::Center).rows);
        }
        let baseline = rows.len() + center.baseline;
        rows.extend(center.pad(width, Align::Center).rows);
        if let Some(bottom) = below {
            rows.extend(bottom.pad(width, Align::Center).rows);
        }
        Self { width, baseline, rows }
    }

    pub fn fraction(top: Self, bottom: Self) -> Self {
        let bar = Self::text("─".repeat(top.width.max(bottom.width).max(1)));
        Self::stack(Some(top), bar, Some(bottom))
    }

    pub fn unicode_script(&self, upper: bool) -> Option<Self> {
        if self.rows.len() != 1 {
            return None;
        }
        let map = if upper { superscript_char } else { subscript_char };
        self.rows[0].chars().map(map).collect::<Option<String>>().map(Self::text)
    }

    pub fn side_scripts(self, upper: Option<Self>, lower: Option<Self>, limit: usize) -> Option<Self> {
        let scripts = match (upper, lower) {
            (None, None) => return Some(self),
            (Some(up), None) => up.unicode_script(true)
                .unwrap_or_else(|| Self::stack(Some(up), Self::text(""), None)),
            (None, Some(down)) => down.unicode_script(false)
                .unwrap_or_else(|| Self::stack(None, Self::text(""), Some(down))),
            (Some(up), Some(down)) => {
                let up = up.unicode_script(true).unwrap_or(up);
                let down = down.unicode_script(false).unwrap_or(down);
                Self::stack(Some(up), Self::text(""), Some(down))
            }
        };
        Self::join(vec![self, scripts], limit)
    }

    pub fn root(self, index: Option<Self>, limit: usize) -> Option<Self> {
        let height = self.rows.len();
        let index = index.map(|index| index.unicode_script(true).unwrap_or(index))
            .unwrap_or_else(|| Self::text(""));
        let prefix = (height + 1).max(index.width);
        let width = prefix + self.width + 1;
        if width > limit {
            return None;
        }

        let baseline = index.rows.len() + self.baseline;
        let mut rows = index.pad(prefix, Align::Right).rows;
        // The index's last row shares the bar. Taller indices extend above it.
        let (bar, above_bar) = rows.split_last_mut()?;
        let right_padding = " ".repeat(self.width + 1);
        for row in above_bar {
            row.push_str(&right_padding);
        }
        bar.push_str(&"_".repeat(self.width + 1));

        let left = " ".repeat(prefix - height - 1);
        for (row, argument) in self.rows.into_iter().enumerate() {
            let rise = height - row - 1;
            let gap = " ".repeat(row);
            if rise == 0 {
                rows.push(format!("{left}╲╱{gap}{argument} "));
            } else {
                let indent = " ".repeat(rise + 1);
                rows.push(format!("{left}{indent}╱{gap}{argument} "));
            }
        }
        Some(Self { width, baseline, rows })
    }

    pub fn enclose(self, open: &str, close: &str, limit: usize) -> Option<Self> {
        if open.is_empty() && close.is_empty() {
            return Some(self);
        }
        let height = self.rows.len();
        let baseline = self.baseline;
        Self::join(vec![
            delimiter(open, height, baseline), self, delimiter(close, height, baseline),
        ], limit)
    }
}

fn delimiter(glyph: &str, height: usize, baseline: usize) -> MathBox {
    if glyph.is_empty() || height == 1 {
        return MathBox::text(glyph);
    }
    let (top, middle, bottom, filler) = match glyph {
        "(" => ("⎛", "⎜", "⎝", "⎜"),
        ")" => ("⎞", "⎟", "⎠", "⎟"),
        "[" => ("⎡", "⎢", "⎣", "⎢"),
        "]" => ("⎤", "⎥", "⎦", "⎥"),
        "{" => ("⎧", "⎨", "⎩", "⎪"),
        "}" => ("⎫", "⎬", "⎭", "⎪"),
        "|" => ("│", "│", "│", "│"),
        _ => (glyph, glyph, glyph, glyph),
    };
    let mut rows = vec![filler.to_string(); height];
    rows[0] = top.into();
    rows[height - 1] = bottom.into();
    if height > 2 {
        rows[height / 2] = middle.into();
    }
    MathBox { width: top.chars().map(|c| cell_width(c, 1)).sum(), baseline, rows }
}

pub(super) fn grid(equations: Vec<Vec<MathBox>>, kind: Grid, limit: usize) -> Option<MathBox> {
    let mut widths = Vec::new();
    for equation in &equations {
        widths.resize(widths.len().max(equation.len()), 0);
        for (width, cell) in widths.iter_mut().zip(equation) {
            *width = (*width).max(cell.width);
        }
    }
    let gaps: Vec<_> = (1..widths.len()).map(|column| match kind {
        Grid::Aligned if column % 2 == 1 => 1,
        _ => 2,
    }).collect();
    let width = widths.iter().sum::<usize>() + gaps.iter().sum::<usize>();
    if width > limit {
        return None;
    }
    let mut rows = Vec::new();
    for equation in equations {
        let mut cells = equation.into_iter();
        let mut parts = Vec::new();
        for (column, cell_width) in widths.iter().copied().enumerate() {
            if column > 0 {
                parts.push(MathBox::text(" ".repeat(gaps[column - 1])));
            }
            let align = match kind {
                Grid::Aligned if column % 2 == 0 => Align::Right,
                Grid::Aligned | Grid::Cases => Align::Left,
                Grid::Centered => Align::Center,
            };
            let cell = cells.next().unwrap_or_else(|| MathBox::text(""));
            parts.push(cell.pad(cell_width, align));
        }
        rows.extend(MathBox::join(parts, limit)?.rows);
    }
    let baseline = (rows.len() - 1) / 2;
    Some(MathBox { width, baseline, rows })
}

pub(super) enum Grid {
    Centered,
    Cases,
    Aligned,
}
