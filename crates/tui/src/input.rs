use crate::render::width;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Default)]
pub struct Input {
    pub text: String,
    /// Byte offset, always on a char boundary.
    cursor: usize,
    /// Earlier prompts, oldest first.
    history: Vec<String>,
    /// How many steps back `recall` has gone; 0 is the text being typed.
    back: usize,
    /// The text being typed, kept while older prompts are shown.
    draft: String,
    /// Where typed prompts are kept across sessions, one per line with `\n` escaped.
    file: Option<std::path::PathBuf>,
}

impl Input {
    /// Applies an editing key; other keys are ignored. Word keys come in the forms terminals
    /// send them: Option-Left is Alt-Left in iTerm2 and kitty but Alt-b in Terminal.app.
    pub fn key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let word = k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.text.len(),
            KeyCode::Backspace if word => {
                let start = self.word_start();
                self.text.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            KeyCode::Char('w') if ctrl => {
                let start = self.word_start();
                self.text.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            KeyCode::Left if word => self.cursor = self.word_start(),
            KeyCode::Char('b') if alt => self.cursor = self.word_start(),
            KeyCode::Right if word => self.cursor = self.word_end(),
            KeyCode::Char('f') if alt => self.cursor = self.word_end(),
            KeyCode::Char(c) if !ctrl => self.insert(&c.to_string()),
            KeyCode::Backspace => {
                if let Some(c) = self.text[..self.cursor].chars().next_back() {
                    self.cursor -= c.len_utf8();
                    self.text.remove(self.cursor);
                }
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                self.text.remove(self.cursor);
            }
            KeyCode::Left => {
                self.cursor -=
                    self.text[..self.cursor].chars().next_back().map_or(0, char::len_utf8)
            }
            KeyCode::Right => {
                self.cursor += self.text[self.cursor..].chars().next().map_or(0, char::len_utf8)
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            _ => {}
        }
    }

    /// Where the word before the cursor starts: back over any gap, then over the word.
    fn word_start(&self) -> usize {
        let before = self.text[..self.cursor].trim_end_matches(|c| !is_word(c));
        before.trim_end_matches(is_word).len()
    }

    /// Where the word after the cursor ends: past any gap, then past the word.
    fn word_end(&self) -> usize {
        let after = self.text[self.cursor..].trim_start_matches(|c| !is_word(c));
        self.text.len() - after.trim_start_matches(is_word).len()
    }

    /// Pasted text keeps its newlines; other control characters are dropped.
    pub fn insert(&mut self, s: &str) {
        let s: String = s
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .chars()
            .filter(|c| *c == '\n' || !c.is_control())
            .collect();
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
    }

    pub fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        if self.history.last().is_none_or(|h| *h != text)
            && let Some(f) = &self.file
        {
            use std::io::Write;
            let line = text.replace('\\', "\\\\").replace('\n', "\\n");
            // shortcut: the file only grows; trim it if it ever gets slow to load.
            let file = std::fs::OpenOptions::new().create(true).append(true).open(f);
            let _ = file.and_then(|mut w| writeln!(w, "{line}"));
        }
        self.remember(&text);
        text
    }

    /// Loads earlier sessions' prompts from `path`, and keeps new ones there.
    pub fn load(&mut self, path: std::path::PathBuf) {
        for line in std::fs::read_to_string(&path).unwrap_or_default().lines() {
            let (mut text, mut esc) = (String::new(), false);
            for c in line.chars() {
                match (esc, c) {
                    (true, 'n') => text.push('\n'),
                    (true, c) => text.push(c),
                    (false, '\\') => {
                        esc = true;
                        continue;
                    }
                    (false, c) => text.push(c),
                }
                esc = false;
            }
            self.remember(&text);
        }
        self.file = Some(path);
    }

    /// The `@word` that ends at the cursor: where it starts, and the text after the `@`.
    pub fn at_word(&self) -> Option<(usize, &str)> {
        let before = &self.text[..self.cursor];
        let start = before
            .rfind(char::is_whitespace)
            .map_or(0, |i| i + before[i..].chars().next().map_or(1, char::len_utf8));
        before[start..].strip_prefix('@').map(|q| (start, q))
    }

    /// Replaces the text from `start` to the cursor with `with`.
    pub fn replace(&mut self, start: usize, with: &str) {
        self.text.replace_range(start..self.cursor, with);
        self.cursor = start + with.len();
    }

    /// Whether the text is an earlier prompt brought back by `recall`.
    pub fn recalling(&self) -> bool {
        self.back > 0
    }

    /// Replaces the text, with the cursor at the end.
    /// A copy with every character shown as a dot, for an API key.
    pub fn masked(&self) -> Input {
        let dots = |s: &str| "•".repeat(s.chars().count());
        let before = dots(&self.text[..self.cursor]);
        let text = before.clone() + &dots(&self.text[self.cursor..]);
        Input { text, cursor: before.len(), ..Input::default() }
    }

    pub fn set(&mut self, text: &str) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    pub fn remember(&mut self, text: &str) {
        if self.history.last().is_none_or(|h| h != text) {
            self.history.push(text.into());
        }
        (self.back, self.cursor) = (0, 0);
    }

    /// Shows the previous prompt (`older`) or the next one, ending at the text being typed.
    pub fn recall(&mut self, older: bool) {
        let back = match older {
            true if self.back < self.history.len() => self.back + 1,
            false if self.back > 0 => self.back - 1,
            _ => return,
        };
        if self.back == 0 {
            self.draft = std::mem::take(&mut self.text);
        }
        self.back = back;
        self.text = match back {
            0 => std::mem::take(&mut self.draft),
            _ => self.history[self.history.len() - back].clone(),
        };
        self.cursor = self.text.len();
    }

    /// Lays the input out as rows of at most `max` columns behind a `> ` prompt, hard-wrapped
    /// so the cursor position is exact. Returns the rows and the cursor's (row, column).
    pub fn layout(&self, max: usize) -> (Vec<String>, (usize, usize)) {
        let mut rows = vec![String::from("> ")];
        let (mut col, mut at) = (2, (0, 2));
        for (i, c) in self.text.char_indices() {
            let w = width(c);
            if c != '\n' && col + w > max {
                rows.push(String::from("  "));
                col = 2;
            }
            if i == self.cursor {
                at = (rows.len() - 1, col);
            }
            if c == '\n' {
                rows.push(String::from("  "));
                col = 2;
            } else {
                rows.last_mut().expect("rows starts non-empty").push(c);
                col += w;
            }
        }
        if self.cursor == self.text.len() {
            if col >= max {
                rows.push(String::from("  "));
                col = 2;
            }
            at = (rows.len() - 1, col);
        }
        (rows, at)
    }
}

/// Words are letters, digits and `_`, so Option-Backspace on `src/main.rs` takes `rs` first,
/// as macOS text fields do.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::Input;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn prompts_carry_over_to_the_next_session() {
        let path = std::env::temp_dir().join(format!("bit-history-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut a = Input::default();
        a.load(path.clone());
        for t in ["one", "two\nlines \\n", "two\nlines \\n"] {
            a.set(t);
            a.take();
        }
        let mut b = Input::default();
        b.load(path.clone());
        b.recall(true);
        assert_eq!(b.text, "two\nlines \\n");
        b.recall(true);
        assert_eq!(b.text, "one");
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2, "no repeats");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn option_keys_move_and_delete_by_word() {
        let alt = |code| KeyEvent::new(code, KeyModifiers::ALT);
        let mut i = Input::default();
        i.insert("run cargo  test_all");
        i.key(alt(KeyCode::Left));
        assert_eq!(i.cursor, 11, "to the start of test_all");
        i.key(alt(KeyCode::Char('b')));
        assert_eq!(i.cursor, 4, "Terminal.app's form, past the double space");
        i.key(alt(KeyCode::Right));
        assert_eq!(i.cursor, 9);
        i.key(alt(KeyCode::Char('f')));
        assert_eq!(i.cursor, 19);
        i.key(alt(KeyCode::Backspace));
        assert_eq!((i.text.as_str(), i.cursor), ("run cargo  ", 11));
        i.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(i.text, "run ");
        i.insert("src/main.rs");
        i.key(alt(KeyCode::Backspace));
        assert_eq!(i.text, "run src/main.");
        i.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        i.key(alt(KeyCode::Backspace));
        assert_eq!((i.text.as_str(), i.cursor), ("run src/.", 8), "deletes only before the cursor");
    }

    #[test]
    fn recall_walks_history_and_keeps_the_draft() {
        let mut i = Input::default();
        for t in ["one", "two", "two"] {
            i.insert(t);
            i.take();
        }
        i.insert("dra");
        let mut seen = Vec::new();
        for older in [true, true, true, false, false, false] {
            i.recall(older);
            seen.push(i.text.clone());
        }
        assert_eq!(seen, ["two", "one", "one", "two", "dra", "dra"]);
    }

    #[test]
    fn at_word_is_the_word_before_the_cursor() {
        let mut i = Input::default();
        i.insert("look at @src/ma");
        assert_eq!(i.at_word(), Some((8, "src/ma")));
        i.replace(8, "@src/main.rs ");
        assert_eq!((i.text.as_str(), i.at_word()), ("look at @src/main.rs ", None));
        i.insert("me@x");
        assert_eq!(i.at_word(), None);
    }
}
