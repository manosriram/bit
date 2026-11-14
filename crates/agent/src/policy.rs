use std::path::PathBuf;

/// Shell syntax that could chain or redirect past an allowlisted command.
const META: &[char] = &[';', '&', '|', '<', '>', '$', '`', '(', ')', '\n'];

/// What may run without asking: allowlisted bash commands plus what the user said
/// "always" to during this session.
pub struct Policy {
    allow: Vec<String>,
    tools: Vec<String>,
    /// The plugins' tools, which ask like `bash` does.
    custom: Vec<String>,
    /// Where `read`, `grep` and `glob` need no approval: the working directory first, then
    /// the directories bit itself points the model at. Empty: anywhere.
    pub roots: Vec<PathBuf>,
}

impl Policy {
    pub fn new(allow: Vec<String>, custom: Vec<String>) -> Self {
        Self { allow, tools: Vec::new(), custom, roots: Vec::new() }
    }

    pub fn check(&self, auto: bool, tool: &str, args: &str) -> bool {
        auto || !tools::mutates(tool)
            && !self.custom.iter().any(|t| t == tool)
            && !self.outside(args)
            || self.tools.iter().any(|t| t == tool)
            || (tool == "bash" && self.allow.iter().any(|a| matches(a, &command(args))))
    }

    /// Whether the call's `path` is outside every root, with `..` and symlinks resolved. A
    /// path that doesn't exist reads nothing, so it isn't asked about.
    fn outside(&self, args: &str) -> bool {
        let Some(cwd) = self.roots.first() else { return false };
        let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
        let Ok(path) = cwd.join(v["path"].as_str().unwrap_or("")).canonicalize() else {
            return false;
        };
        !self.roots.iter().any(|r| r.canonicalize().is_ok_and(|r| path.starts_with(r)))
    }

    /// Bash is remembered per command, so "always" for `ls` doesn't allow `rm`.
    pub fn always(&mut self, tool: &str, args: &str) {
        if tool == "bash" {
            self.allow.push(command(args));
        } else {
            self.tools.push(tool.into());
        }
    }
}

fn command(args: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
    v["command"].as_str().unwrap_or("").trim().into()
}

fn matches(entry: &str, cmd: &str) -> bool {
    cmd == entry
        || cmd
            .strip_prefix(entry)
            .and_then(|rest| rest.strip_prefix(' '))
            .is_some_and(|rest| !rest.contains(META))
}

#[cfg(test)]
mod tests {
    use super::Policy;

    fn bash(c: &str) -> String {
        serde_json::json!({ "command": c }).to_string()
    }

    #[test]
    fn allowlist_extends_only_with_plain_arguments() {
        let p = Policy::new(vec!["cargo test".into()], vec!["deploy".into()]);
        let ok = |c: &str| p.check(false, "bash", &bash(c));
        assert!(ok("cargo test"));
        assert!(ok("cargo test -p llm"));
        assert!(!ok("cargo testx"));
        assert!(!ok("cargo test; rm -rf ~"));
        assert!(!ok("cargo test && rm -rf ~"));
        assert!(!ok("cargo test $(rm -rf ~)"));
        assert!(!ok("cargo test\nrm -rf ~"));
        assert!(p.check(false, "read", "{}"));
        assert!(!p.check(false, "edit", "{}"));
        assert!(p.check(true, "edit", "{}"));
        assert!(!p.check(false, "deploy", "{}"));
        assert!(p.check(true, "deploy", "{}"));
    }

    #[test]
    fn reads_outside_the_roots_ask() {
        let dir = std::env::temp_dir().join(format!("bit-policy-{}", std::process::id()));
        let (cwd, spill, other) = (dir.join("cwd"), dir.join("spill"), dir.join("other"));
        for d in [&cwd, &spill, &other] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("f"), "").unwrap();
        }
        std::os::unix::fs::symlink(&other, cwd.join("link")).unwrap();
        let mut p = Policy::new(Vec::new(), Vec::new());
        p.roots = vec![cwd, spill.clone()];
        let ok = |p: &Policy, tool: &str, path: &str| {
            p.check(false, tool, &serde_json::json!({ "path": path }).to_string())
        };
        assert!(ok(&p, "read", "f") && ok(&p, "read", "missing") && p.check(false, "grep", "{}"));
        assert!(ok(&p, "read", spill.join("f").to_str().unwrap()));
        assert!(!ok(&p, "read", "../other/f") && !ok(&p, "read", "link/f"));
        assert!(!ok(&p, "read", other.join("f").to_str().unwrap()) && !ok(&p, "grep", ".."));
        assert!(p.check(true, "read", r#"{"path":"../other/f"}"#));
        p.always("read", "{}");
        assert!(ok(&p, "read", "../other/f") && !ok(&p, "grep", ".."));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn always_is_per_command_for_bash_and_per_tool_otherwise() {
        let mut p = Policy::new(Vec::new(), Vec::new());
        p.always("bash", &bash("ls"));
        p.always("edit", "{}");
        assert!(p.check(false, "bash", &bash("ls")));
        assert!(!p.check(false, "bash", &bash("rm x")));
        assert!(p.check(false, "edit", r#"{"path":"any"}"#));
        assert!(!p.check(false, "write", "{}"));
    }
}
