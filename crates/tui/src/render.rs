use std::fmt::Write as _;
use std::io::{self, Write};
use std::ops::Range;

/// Columns a char takes. An approximation that covers CJK and common emoji without a
/// width table; only rows in the live region need exact widths, since scrollback is never
/// redrawn.
pub fn width(c: char) -> usize {
    match c as u32 {
        0x300..=0x36F | 0x200B..=0x200F | 0xFE00..=0xFE0F => 0,
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

pub fn str_width(s: &str) -> usize {
    s.chars().map(width).sum()
}

/// Greedy word wrap into rows of at most `max` columns. Greedy means every row but the
/// last is final while text is appended, so finished rows can go to scrollback early.
pub fn wrap(s: &str, max: usize) -> Vec<Range<usize>> {
    let mut rows = Vec::new();
    let (mut start, mut col, mut space) = (0, 0, 0);
    for (i, c) in s.char_indices() {
        let w = width(c);
        if col + w > max && i > start {
            let end = if space > start { space } else { i };
            rows.push(start..end);
            start = end;
            col = str_width(&s[start..i]);
        }
        col += w;
        if c == ' ' {
            space = i + 1;
        }
    }
    rows.push(start..s.len());
    rows
}

/// Builds one terminal line, tracking its width so borders can be filled to the edge.
pub struct Line {
    pub text: String,
    pub width: usize,
    color: bool,
}

impl Line {
    pub fn new(color: bool) -> Self {
        Self {
            text: String::new(),
            width: 0,
            color,
        }
    }

    /// `sgr` is an SGR parameter list such as "33" or "1"; empty means plain.
    pub fn add(&mut self, sgr: &str, s: &str) -> &mut Self {
        if self.color && !sgr.is_empty() {
            let _ = write!(self.text, "\x1b[{sgr}m{s}\x1b[0m");
        } else {
            self.text.push_str(s);
        }
        self.width += str_width(s);
        self
    }

    pub fn fill(&mut self, sgr: &str, c: char, to: usize) -> &mut Self {
        let n = to.saturating_sub(self.width);
        self.add(sgr, &c.to_string().repeat(n))
    }

    pub fn take(&mut self) -> String {
        self.width = 0;
        std::mem::take(&mut self.text)
    }
}

/// The live region, redrawn by diffing rows against the previous frame. Finished rows are
/// written above it and scroll away into the terminal's own scrollback.
#[derive(Default)]
pub struct Term {
    prev: Vec<String>,
    /// Row of the cursor within the region.
    row: usize,
}

impl Term {
    pub fn draw(
        &mut self,
        done: &[String],
        frame: &[String],
        cursor: (usize, usize),
    ) -> io::Result<()> {
        let mut out = String::from("\x1b[?25l");
        self.top(&mut out);
        if !done.is_empty() {
            out.push_str("\x1b[J");
            for l in done {
                out.push_str(l);
                out.push_str("\r\n");
            }
            self.prev.clear();
        }
        for (i, l) in frame.iter().enumerate() {
            if self.prev.get(i) != Some(l) {
                out.push_str("\x1b[2K");
                out.push_str(l);
            }
            if i + 1 < frame.len() {
                out.push_str("\r\n");
            }
        }
        if self.prev.len() > frame.len() {
            out.push_str("\r\n\x1b[J\x1b[A");
        }
        let up = frame.len().saturating_sub(1 + cursor.0);
        if up > 0 {
            let _ = write!(out, "\x1b[{up}A");
        }
        let _ = write!(out, "\x1b[{}G\x1b[?25h", cursor.1 + 1);
        self.row = cursor.0;
        self.prev = frame.to_vec();
        let mut stdout = io::stdout().lock();
        stdout.write_all(out.as_bytes())?;
        stdout.flush()
    }

    /// Removes the live region, leaving only scrollback.
    pub fn clear(&mut self) -> io::Result<()> {
        let mut out = String::new();
        self.top(&mut out);
        out.push_str("\x1b[J\x1b[?25h");
        self.prev.clear();
        self.row = 0;
        let mut stdout = io::stdout().lock();
        stdout.write_all(out.as_bytes())?;
        stdout.flush()
    }

    fn top(&self, out: &mut String) {
        if self.row > 0 {
            let _ = write!(out, "\x1b[{}A", self.row);
        }
        out.push('\r');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(s: &str, max: usize) -> Vec<&str> {
        wrap(s, max).into_iter().map(|r| &s[r]).collect()
    }

    #[test]
    fn wraps_at_spaces_then_hard() {
        assert_eq!(rows("aaa bbb ccc", 7), ["aaa ", "bbb ccc"]);
        assert_eq!(rows("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(rows("世界世界", 4), ["世界", "世界"]);
        assert_eq!(rows("", 4), [""]);
    }

    #[test]
    fn finished_rows_are_stable_while_appending() {
        let full = "the quick brown fox jumps over the lazy dog again and again";
        for end in 1..=full.len() {
            let a = rows(&full[..end], 12);
            let b = rows(full, 12);
            assert_eq!(a[..a.len() - 1], b[..a.len() - 1], "prefix {end}");
        }
    }
}
