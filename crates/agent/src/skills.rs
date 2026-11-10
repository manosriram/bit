use std::path::{Path, PathBuf};

/// A skill is knowledge for the model: a folder with a `SKILL.md` file. Only the name and
/// description are injected into the prompt; the model reads the body on demand with the
/// existing `read` tool. That keeps the per-turn cost to one line per skill.
#[derive(Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

impl Skill {
    fn new(name: String, description: String, path: PathBuf) -> Self {
        Self { name, description, path }
    }
}

/// Every skill under `dir`, sorted by name so the prompt section is stable.
pub fn list(dir: &Path) -> Vec<Skill> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let md = path.join("SKILL.md");
        let Ok(text) = std::fs::read_to_string(&md) else { continue };
        let Some((name, description)) = frontmatter(&text) else { continue };
        out.push(Skill::new(name, description, md));
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The prompt section that lists installed skills. Empty when there are none, so it costs
/// nothing when the user has no skills.
pub fn section(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut s = "\nSkills (read a skill's SKILL.md before doing a task it covers):\n".to_owned();
    for k in skills {
        s += &format!("- {} — {} ({})\n", k.name, k.description, k.path.display());
    }
    s
}

/// Parses a simple `name` / `description` frontmatter. SKILL.md is expected to start with
/// `---`, contain `name:` and `description:` lines, and end with `---`; if it doesn't, the
/// folder name and first non-empty line are used as a fallback.
pub fn parse(text: &str) -> Option<(String, String)> {
    frontmatter(text)
}

/// Installs a skill from a git repo into `dir`. Returns a message to show the user, or an
/// error string. This is synchronous so it can run inside `spawn_blocking` in the agent or
/// directly in the CLI.
pub fn add(dir: &Path, repo: &str) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let url = normalize_repo(repo);
    let name = skill_name(&url);
    let path = dir.join(&name);
    if path.exists() {
        return Err(format!(
            "skill `{name}` already exists. hint: bit skills update {name} or /skills update {name}"
        ));
    }
    let out = std::process::Command::new("git")
        .args(["clone", "--depth", "1", &url, path.to_str().unwrap_or(&name)])
        .output()
        .map_err(|e| format!("git clone failed: {e}. hint: install git"))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&path);
        return Err(format!("git clone failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let md = path.join("SKILL.md");
    let text = std::fs::read_to_string(&md)
        .map_err(|e| format!("{}: {e}. hint: a skill needs a SKILL.md", md.display()))?;
    let (n, description) = parse(&text)
        .ok_or_else(|| format!("{} is missing a name/description frontmatter", md.display()))?;
    let mut msg = format!("added {name}: {description}");
    if n != name {
        msg += &format!(". warning: folder is `{name}` but frontmatter names it `{n}`");
    }
    Ok(msg)
}

fn normalize_repo(repo: &str) -> String {
    if repo.contains("://") || repo.starts_with("git@") {
        repo.into()
    } else if repo.contains('/') {
        format!("https://github.com/{repo}.git")
    } else {
        format!("https://github.com/{repo}/{repo}.git")
    }
}

fn skill_name(url: &str) -> String {
    let base = url.rsplit('/').next().unwrap_or(url);
    base.strip_suffix(".git").unwrap_or(base).into()
}

fn frontmatter(text: &str) -> Option<(String, String)> {
    let mut lines = text.lines();
    if lines.next()? != "---" {
        return None;
    }
    let (mut name, mut description) = (None, None);
    for line in lines {
        if line == "---" {
            break;
        }
        let (k, v) = line.split_once(':')?;
        let v = v.trim();
        match k.trim() {
            "name" => name = Some(v.to_owned()),
            "description" => description = Some(v.to_owned()),
            _ => {}
        }
    }
    Some((name?, description.unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let md = "---\nname: pdf-export\ndescription: Extract tables from PDFs\n---\nDo this...";
        assert_eq!(
            frontmatter(md).unwrap(),
            ("pdf-export".into(), "Extract tables from PDFs".into())
        );
    }

    #[test]
    fn missing_frontmatter_is_none() {
        assert!(frontmatter("no frontmatter").is_none());
    }

    #[test]
    fn empty_skills_dir_returns_empty() {
        let dir = std::env::temp_dir().join(format!("bit-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(list(&dir).is_empty());
    }

    #[test]
    fn normalizes_short_repo() {
        assert_eq!(normalize_repo("owner/repo"), "https://github.com/owner/repo.git");
        assert_eq!(skill_name("https://github.com/owner/repo.git"), "repo");
    }
}
