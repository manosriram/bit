/// Shell syntax that could chain or redirect past an allowlisted command.
const META: &[char] = &[';', '&', '|', '<', '>', '$', '`', '(', ')', '\n'];

/// What may run without asking: allowlisted bash commands plus what the user said
/// "always" to during this session.
pub struct Policy {
    allow: Vec<String>,
    tools: Vec<String>,
    /// The plugins' tools, which ask like `bash` does.
    custom: Vec<String>,
}

impl Policy {
    pub fn new(allow: Vec<String>, custom: Vec<String>) -> Self {
        Self { allow, tools: Vec::new(), custom }
    }

    pub fn check(&self, auto: bool, tool: &str, args: &str) -> bool {
        auto || !tools::mutates(tool) && !self.custom.iter().any(|t| t == tool)
            || self.tools.iter().any(|t| t == tool)
            || (tool == "bash" && self.allow.iter().any(|a| matches(a, &command(args))))
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
