use crate::DIM;
use crate::render::{Line, clip, str_width, wrap};
use std::ops::Range;

/// Markdown state that carries from one line to the next: only fenced code does.
#[derive(Default)]
pub struct Markdown {
    code: bool,
}

impl Markdown {
    /// The rows for one source line, each at most `width` columns, with the two-column
    /// indent of model text. Rendering a prefix of a line gives the same rows, except the
    /// last, as rendering the whole line, so rows can go to scrollback while it streams.
    pub fn rows(&self, line: &str, width: usize, color: bool) -> Vec<String> {
        let mut l = Line::new(color);
        if let Some(lang) = line.trim_start().strip_prefix("```") {
            return match (self.code, lang.trim()) {
                (true, _) | (false, "") => Vec::new(),
                (false, lang) => vec![l.add(DIM, &format!("  {}", clip(lang, width - 2))).take()],
            };
        }
        if self.code {
            let rows = wrap(line, width - 4).into_iter();
            return rows.map(|r| l.add(DIM, "  | ").add("", line[r].trim_end()).take()).collect();
        }
        let (lead, text, heading) = block(line);
        let (vis, marks) = inline(text, heading);
        let hang = " ".repeat(str_width(&lead));
        let rows = wrap(&vis, width.saturating_sub(2 + hang.len()).max(1));
        let rows = rows.into_iter().enumerate().map(|(i, r)| {
            l.add("", "  ").add("", if i == 0 { &lead } else { &hang });
            styled(&mut l, &vis, r, &marks);
            l.take()
        });
        rows.collect()
    }

    /// Called once a line is complete: a fence line opens or closes a code block.
    pub fn end(&mut self, line: &str) {
        if line.trim_start().starts_with("```") {
            self.code = !self.code;
        }
    }
}

/// Splits off a heading or list marker: (bullet to print, the rest, whether it's a heading).
fn block(line: &str) -> (String, &str, bool) {
    let t = line.trim_start();
    let indent = &line[..line.len() - t.len()];
    let hashes = t.len() - t.trim_start_matches('#').len();
    if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
        return (String::new(), t[hashes..].trim_start(), true);
    }
    if let Some(rest) = t.strip_prefix(['-', '*', '+']).and_then(|r| r.strip_prefix(' ')) {
        return (format!("{indent}- "), rest, false);
    }
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let after = &t[digits..];
    if digits > 0 && (after.starts_with(". ") || after.starts_with(") ")) {
        return (format!("{indent}{} ", &t[..=digits]), &after[2..], false);
    }
    (String::new(), line, false)
}

/// The visible text with `**` and backticks removed, and where each style starts in it.
fn inline(text: &str, heading: bool) -> (String, Vec<(usize, &'static str)>) {
    let (mut vis, mut marks) = (String::new(), Vec::new());
    let (mut bold, mut code, mut rest) = (false, false, text);
    loop {
        let sgr = if code {
            "36"
        } else if bold || heading {
            "1"
        } else {
            ""
        };
        if marks.last().is_none_or(|m: &(usize, &str)| m.1 != sgr) {
            marks.push((vis.len(), sgr));
        }
        let Some(c) = rest.chars().next() else { break };
        if c == '`' {
            code = !code;
            rest = &rest[1..];
        } else if !code && rest.starts_with("**") {
            bold = !bold;
            rest = &rest[2..];
        } else {
            vis.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    (vis, marks)
}

/// Adds `vis[r]`, without trailing spaces, styled by `marks`.
fn styled(l: &mut Line, vis: &str, r: Range<usize>, marks: &[(usize, &str)]) {
    let end = r.start + vis[r.clone()].trim_end().len();
    for (i, &(at, sgr)) in marks.iter().enumerate() {
        let next = marks.get(i + 1).map_or(vis.len(), |m| m.0);
        let (a, b) = (at.max(r.start), next.min(end));
        if a < b {
            l.add(sgr, &vis[a..b]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Markdown;

    fn render(src: &str, width: usize) -> Vec<String> {
        let mut md = Markdown::default();
        let mut out = Vec::new();
        for line in src.lines() {
            out.extend(md.rows(line, width, false));
            md.end(line);
        }
        out
    }

    #[test]
    fn blocks_and_inline() {
        let src = "# Title\nsome **bold** and `a ** b`\n- one two three four\n2. next\n```rust\nlet x = 1;\n```\nafter";
        let want = [
            "  Title",
            "  some bold",
            "  and a ** b",
            "  - one two",
            "    three four",
            "  2. next",
            "  rust",
            "  | let x = 1;",
            "  after",
        ];
        assert_eq!(render(src, 14), want);
        let md = Markdown::default();
        assert_eq!(md.rows("**hi** `x`", 20, true)[0], "  \x1b[1mhi\x1b[0m \x1b[36mx\x1b[0m");
    }

    #[test]
    fn prefixes_keep_the_finished_rows() {
        let md = Markdown::default();
        let full = "- a **bold claim** with `code in it` and more words to wrap around";
        let all = md.rows(full, 20, true);
        for end in (1..=full.len()).filter(|&e| full.is_char_boundary(e)) {
            let rows = md.rows(&full[..end], 20, true);
            assert_eq!(rows[..rows.len() - 1], all[..rows.len() - 1], "prefix {end}");
        }
    }
}
