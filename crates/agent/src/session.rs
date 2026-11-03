use serde::Deserialize;
use serde_json::value::RawValue;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// The append-only log of one conversation.
pub struct Session {
    path: PathBuf,
    /// The working directory for the header line, held until the first write so an empty
    /// session leaves no file.
    header: Option<PathBuf>,
    provider: String,
    model: String,
    file: Option<File>,
    /// How many of the context's messages are already in the file.
    pub(crate) logged: usize,
}

pub struct Loaded {
    pub session: Session,
    pub messages: Vec<Box<RawValue>>,
}

#[derive(Deserialize)]
struct Record<'a> {
    t: &'a str,
    #[serde(borrow)]
    msg: Option<&'a RawValue>,
    cwd: Option<PathBuf>,
    provider: Option<String>,
    model: Option<String>,
}

impl Session {
    pub fn new(dir: &Path, id: &str, cwd: &Path, provider: &str, model: &str) -> Self {
        let path = dir.join(format!("{id}.jsonl"));
        let (provider, model) = (provider.into(), model.into());
        Self { path, header: Some(cwd.into()), provider, model, file: None, logged: 0 }
    }

    pub fn open(path: &Path) -> io::Result<Loaded> {
        let (mut messages, mut provider, mut model) = (Vec::new(), String::new(), String::new());
        for line in BufReader::new(File::open(path)?).lines() {
            let line = line?;
            // A crash can tear the last line; everything before it is still good.
            let Ok(r) = serde_json::from_str::<Record>(&line) else {
                continue;
            };
            match (r.t, r.msg) {
                ("header" | "model", _) => {
                    (provider, model) =
                        (r.provider.unwrap_or_default(), r.model.unwrap_or_default())
                }
                ("clear", _) => messages.clear(),
                ("compact", Some(m)) => messages = vec![m.to_owned()],
                ("msg", Some(m)) => messages.push(m.to_owned()),
                _ => {}
            }
        }
        let (path, logged) = (path.into(), messages.len());
        let session = Self { path, header: None, provider, model, file: None, logged };
        Ok(Loaded { session, messages })
    }

    /// The provider of the last header or `model` record; empty in logs from before providers.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// The model of the last header or `model` record.
    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn id(&self) -> &str {
        self.path.file_stem().and_then(|s| s.to_str()).unwrap_or("")
    }

    pub(crate) fn append(&mut self, messages: &[Box<RawValue>]) -> io::Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let lines: String =
            messages.iter().map(|m| format!("{{\"t\":\"msg\",\"msg\":{}}}\n", m.get())).collect();
        self.write(&lines)?;
        self.logged += messages.len();
        Ok(())
    }

    pub(crate) fn usage(&mut self, u: &llm::Usage) -> io::Result<()> {
        let (input, cached, output) = (u.input, u.cached, u.output);
        let r =
            serde_json::json!({"t": "usage", "input": input, "cached": cached, "output": output});
        self.write(&format!("{r}\n"))
    }

    /// Before the first write the header just changes; after it, a `model` record is added.
    pub(crate) fn set_model(&mut self, provider: &str, model: &str) -> io::Result<()> {
        (self.provider, self.model) = (provider.into(), model.into());
        if self.header.is_some() {
            return Ok(());
        }
        let r = serde_json::json!({"t": "model", "provider": provider, "model": model});
        self.write(&format!("{r}\n"))
    }

    /// Starts the history over; a resume then sees only what comes after.
    pub(crate) fn clear(&mut self) -> io::Result<()> {
        self.logged = 0;
        match self.header {
            Some(_) => Ok(()),
            None => self.write("{\"t\":\"clear\"}\n"),
        }
    }

    /// Records that the history was replaced by `summary`.
    pub(crate) fn compact(&mut self, summary: &RawValue) -> io::Result<()> {
        self.logged = 1;
        self.write(&format!("{{\"t\":\"compact\",\"msg\":{}}}\n", summary.get()))
    }

    fn write(&mut self, lines: &str) -> io::Result<()> {
        let header = self.header.take().map(|cwd| {
            let (p, m) = (&self.provider, &self.model);
            let h =
                serde_json::json!({"t": "header", "v": 1, "cwd": cwd, "provider": p, "model": m});
            format!("{h}\n")
        });
        let out = header.unwrap_or_default() + lines;
        let file = match &mut self.file {
            Some(f) => f,
            None => {
                if let Some(dir) = self.path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let f = OpenOptions::new().create(true).append(true).open(&self.path)?;
                self.file.insert(f)
            }
        };
        file.write_all(out.as_bytes())
    }

    pub(crate) fn sync(&self) -> io::Result<()> {
        self.file.as_ref().map_or(Ok(()), File::sync_data)
    }
}

/// The most recently changed session started in `cwd`.
pub fn latest(dir: &Path, cwd: &Path) -> io::Result<Option<PathBuf>> {
    let mut found: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(dir)? {
        let path = e?.path();
        let Ok(time) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
            continue;
        };
        if found.as_ref().is_some_and(|(t, _)| *t >= time) {
            continue;
        }
        let mut first = String::new();
        let Ok(f) = File::open(&path) else { continue };
        BufReader::new(f).read_line(&mut first)?;
        let r: Option<Record> = serde_json::from_str(&first).ok();
        if r.is_some_and(|r| r.t == "header" && r.cwd.as_deref() == Some(cwd)) {
            found = Some((time, path));
        }
    }
    Ok(found.map(|(_, p)| p))
}

/// The one session whose id starts with `prefix`.
pub fn find(dir: &Path, prefix: &str) -> Result<PathBuf, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut hits = entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(prefix) && n.ends_with(".jsonl"))
    });
    match (hits.next(), hits.next()) {
        (Some(p), None) => Ok(p),
        (None, _) => Err(format!("no session starts with `{prefix}`")),
        (Some(_), Some(_)) => Err(format!("more than one session starts with `{prefix}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_lazily_and_reads_back_exact_bytes() {
        let dir = std::env::temp_dir().join(format!("bit-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = Session::new(&dir, "abc123", Path::new("/w"), "", "");
        s.append(&[]).unwrap();
        s.set_model("p1", "m1").unwrap();
        assert!(!dir.exists());
        // Odd spacing and key order must survive: the cache depends on the exact bytes.
        let msgs: Vec<Box<RawValue>> =
            [r#"{"role":"user", "content":"hi"}"#, r#"{"content":"yo","role":"assistant"}"#]
                .iter()
                .map(|m| RawValue::from_string((*m).into()).unwrap())
                .collect();
        s.append(&msgs[..1]).unwrap();
        let u = llm::Usage::default();
        s.usage(&u).unwrap();
        s.append(&msgs[1..]).unwrap();
        s.set_model("p2", "m2").unwrap();
        s.usage(&u).unwrap();
        s.sync().unwrap();
        // A torn line and an unknown record type are skipped.
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("abc123.jsonl"))
            .unwrap()
            .write_all(b"{\"t\":\"later\",\"x\":1}\n{\"t\":\"msg\",\"msg\":{\"ro")
            .unwrap();

        let log = std::fs::read_to_string(dir.join("abc123.jsonl")).unwrap();
        let header = r#"{"cwd":"/w","model":"m1","provider":"p1","t":"header","v":1}"#;
        assert!(log.starts_with(header), "{log}");
        let l = Session::open(&find(&dir, "abc").unwrap()).unwrap();
        assert_eq!((l.session.provider(), l.session.model()), ("p2", "m2"));
        assert_eq!(l.session.id(), "abc123");
        assert_eq!(l.session.logged, 2);
        let got: Vec<&str> = l.messages.iter().map(|m| m.get()).collect();
        assert_eq!(got, [msgs[0].get(), msgs[1].get()]);

        assert_eq!(latest(&dir, Path::new("/w")).unwrap(), Some(dir.join("abc123.jsonl")));
        assert_eq!(latest(&dir, Path::new("/other")).unwrap(), None);
        assert!(find(&dir, "zz").is_err());
    }

    #[test]
    fn clear_drops_earlier_messages_on_load() {
        let dir = std::env::temp_dir().join(format!("bit-session-clear-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let m = |s: &str| RawValue::from_string(s.into()).unwrap();
        let mut s = Session::new(&dir, "c1", Path::new("/w"), "p", "m");
        s.clear().unwrap();
        s.append(&[m("1")]).unwrap();
        s.clear().unwrap();
        assert_eq!(s.logged, 0);
        s.append(&[m("2")]).unwrap();
        let l = Session::open(&dir.join("c1.jsonl")).unwrap();
        assert_eq!(l.messages.iter().map(|m| m.get()).collect::<Vec<_>>(), ["2"]);
    }
}
