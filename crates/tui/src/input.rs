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
}

impl Input {
    /// Applies an editing key; other keys are ignored.
    pub fn key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.text.len(),
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
        self.remember(&text);
        text
    }

    /// Replaces the text, with the cursor at the end.
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

#[cfg(test)]
mod tests {
    use super::Input;

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
}
