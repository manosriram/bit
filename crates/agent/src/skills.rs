use std::fs;
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

/// Every skill under `dir`, sorted by name so the prompt section is stable.
pub fn list(dir: &Path) -> Vec<Skill> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let md = path.join("SKILL.md");
        let Ok(text) = fs::read_to_string(&md) else { continue };
        let Some((name, description)) = frontmatter(&text) else { continue };
        out.push(Skill { name, description, path: md });
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
    let mut s = "\nSkills (read a skill's SKILL.md before doing a task it covers; a message starting with /<name> asks for that skill):\n".to_owned();
    for k in skills {
        s += &format!("- {} — {} ({})\n", k.name, k.description, k.path.display());
    }
    s
}

/// Installs a skill from a git repo into `dir`. Returns a message to show the user, or an
/// error string. This is synchronous so it can run inside `spawn_blocking` in the agent or
/// directly in the CLI.
///
/// Two repo layouts are accepted:
/// - A single skill: `SKILL.md` at the repo root.
/// - A multi-skill repo: a `skills/` directory where each subdirectory has its own
///   `SKILL.md`. Each sub-skill is installed separately under `dir` and gets a `.source`
///   file so updates can find their origin later.
pub fn add(dir: &Path, repo: &str) -> Result<String, String> {
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let url = normalize_repo(repo);
    let name = skill_name(&url);
    let tmp = dir.join(format!(".tmp-{name}"));
    let _ = fs::remove_dir_all(&tmp);
    let out = std::process::Command::new("git")
        .args(["clone", "--depth", "1", &url, tmp.to_str().unwrap_or(&name)])
        .output()
        .map_err(|e| format!("git clone failed: {e}. hint: install git"))?;
    if !out.status.success() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!("git clone failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let result = if tmp.join("SKILL.md").is_file() {
        install_single(dir, &tmp, &url, &name)
    } else if tmp.join("skills").is_dir() {
        install_multi(dir, &tmp, &url)
    } else {
        Err("no SKILL.md at root and no skills/ directory found. hint: the repo should contain a SKILL.md or a skills/ folder with sub-skills".into())
    };
    let _ = fs::remove_dir_all(&tmp);
    result
}

fn install_single(dir: &Path, tmp: &Path, url: &str, name: &str) -> Result<String, String> {
    let md = tmp.join("SKILL.md");
    let text = fs::read_to_string(&md).map_err(|e| format!("{}: {e}", md.display()))?;
    let (n, description) = frontmatter(&text)
        .ok_or_else(|| format!("{} is missing a name/description frontmatter", md.display()))?;
    let path = dir.join(name);
    if path.exists() {
        return Err(format!(
            "skill `{name}` already exists. hint: bit skills update {name} or /skills update {name}"
        ));
    }
    fs::rename(tmp, &path).map_err(|e| format!("could not move skill into place: {e}"))?;
    write_source(&path, url, "")?;
    let mut msg = format!("added {name}: {description}");
    if n != name {
        msg += &format!(". warning: folder is `{name}` but frontmatter names it `{n}`");
    }
    Ok(msg)
}

fn install_multi(dir: &Path, tmp: &Path, url: &str) -> Result<String, String> {
    let skills_dir = tmp.join("skills");
    let mut skills = Vec::new();
    for e in fs::read_dir(&skills_dir).map_err(|e| format!("skills/: {e}"))? {
        let e = e.map_err(|e| format!("skills/: {e}"))?;
        let src = e.path();
        if !src.is_dir() {
            continue;
        }
        let md = src.join("SKILL.md");
        if !md.is_file() {
            continue;
        }
        let text = fs::read_to_string(&md).map_err(|e| format!("{}: {e}", md.display()))?;
        let (name, description) = frontmatter(&text)
            .ok_or_else(|| format!("{} is missing a name/description frontmatter", md.display()))?;
        let target = dir.join(&name);
        if target.exists() {
            return Err(format!(
                "skill `{name}` already exists. hint: bit skills update {name} or /skills update {name}"
            ));
        }
        skills.push((name, description, src, target));
    }
    if skills.is_empty() {
        return Err("skills/ directory contains no sub-skills with SKILL.md".into());
    }
    let count = skills.len();
    let mut lines = Vec::new();
    for (name, description, src, target) in skills {
        fs::rename(&src, &target).map_err(|e| format!("could not move skill `{name}`: {e}"))?;
        write_source(&target, url, &format!("skills/{name}"))?;
        lines.push(format!("  {name} — {description}"));
    }
    Ok(format!("added {count} skills:\n{}", lines.join("\n")))
}

fn write_source(path: &Path, url: &str, subpath: &str) -> Result<(), String> {
    let text = format!("{url}\n{subpath}\n");
    fs::write(path.join(".source"), text).map_err(|e| format!("could not write .source: {e}"))
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

/// A multi-line YAML scalar, as `description:` uses it: folded (`>`) joins lines with
/// spaces and a blank line becomes a paragraph break; literal (`|`) keeps the line breaks.
/// The chomping indicators (`-`, `+`) only affect trailing newlines, which are trimmed
/// away at the end, so they are accepted but not tracked.
#[derive(Clone, Copy)]
enum Scalar {
    Folded,
    Literal,
}

/// `description: >`, `description: >- start here`, `description: |+` and so on. Returns the
/// scalar kind and the inline text after the indicator, if any.
fn scalar_indicator(v: &str) -> Option<(Scalar, &str)> {
    for (prefix, kind) in [
        (">-", Scalar::Folded),
        (">+", Scalar::Folded),
        (">", Scalar::Folded),
        ("|-", Scalar::Literal),
        ("|+", Scalar::Literal),
        ("|", Scalar::Literal),
    ] {
        if let Some(rest) = v.strip_prefix(prefix) {
            return Some((kind, rest.trim()));
        }
    }
    None
}

/// Parses a simple `name` / `description` frontmatter. SKILL.md is expected to start with
/// `---`, contain `name:` and `description:` lines, and end with `---`. `description` may
/// use a multi-line YAML scalar (`>`, `|-` and friends); everything else must be one line.
fn frontmatter(text: &str) -> Option<(String, String)> {
    let mut lines = text.lines();
    if lines.next()? != "---" {
        return None;
    }
    let (mut name, mut description) = (None, None);
    let mut scalar: Option<(Scalar, String)> = None;

    for line in lines {
        if line == "---" {
            break;
        }
        if let Some((kind, content)) = &mut scalar {
            // A blank (whitespace-only) line is a paragraph break.
            if line.trim().is_empty() {
                content.push('\n');
                continue;
            }
            // An indented line continues the scalar.
            if line.starts_with(' ') || line.starts_with('\t') {
                match kind {
                    Scalar::Folded if !content.is_empty() && !content.ends_with('\n') => {
                        content.push(' ');
                    }
                    Scalar::Literal => content.push('\n'),
                    Scalar::Folded => {}
                }
                content.push_str(line.trim());
                continue;
            }
            // A line at the top level ends the scalar; fall through to parse it as a key.
            description = Some(content.trim().to_owned());
            scalar = None;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        match k.trim() {
            "name" => name = Some(v.trim().to_owned()),
            "description" => match scalar_indicator(v.trim()) {
                Some((kind, inline)) => {
                    scalar = Some((kind, inline.to_owned()));
                }
                None => description = Some(v.trim().to_owned()),
            },
            _ => {}
        }
    }
    if let Some((_, content)) = scalar {
        description = Some(content.trim().to_owned());
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
    fn parses_folded_multiline_description() {
        let md = "---\nname: ponytail\ndescription: >\n  Lazy senior dev mode: the smallest change\n  that fully solves the task.\nargument-hint: \"[lite|full|ultra|off]\"\nlicense: MIT\n---\n# Ponytail\n";
        let (name, desc) = frontmatter(md).unwrap();
        assert_eq!(name, "ponytail");
        assert!(desc.contains("Lazy senior dev mode"));
        assert!(desc.contains("fully solves the task"));
        assert!(!desc.contains("argument-hint"));
        assert!(!desc.contains("license"));
    }

    #[test]
    fn parses_every_multiline_scalar_indicator() {
        // Chomping indicators differ only in trailing newlines, which are trimmed.
        for indicator in [">", ">-", ">+", "|", "|-", "|+"] {
            let md = format!("---\nname: s\ndescription: {indicator}\n  line 1\n  line 2\n---\n");
            let (_, desc) = frontmatter(&md).unwrap();
            assert_eq!(desc.replace('\n', " "), "line 1 line 2", "indicator {indicator}");
        }
        // An inline start followed by folded lines joins with a space.
        let md = "---\nname: s\ndescription: > start\n  here\n---\n";
        assert_eq!(frontmatter(md).unwrap().1, "start here");
    }

    #[test]
    fn blank_lines_are_paragraph_breaks() {
        // Folded: a blank line becomes one newline; whitespace-only lines count as blank.
        let md = "---\nname: s\ndescription: >\n  one\n   \n  two\n---\n";
        assert_eq!(frontmatter(md).unwrap().1, "one\ntwo");
        // Literal: lines keep their breaks; a blank line is an empty line between them.
        let md = "---\nname: s\ndescription: |\n  one\n\n  two\n---\n";
        assert_eq!(frontmatter(md).unwrap().1, "one\n\ntwo");
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
