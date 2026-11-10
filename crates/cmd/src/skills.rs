use agent::skills::list;
use std::path::Path;
use std::process::Command;

const FIX: &str = "hint: install git, or clone the repo into ~/.config/bit/skills/<name>";

pub fn run(sub: &[String], dir: &Path) -> Result<(), String> {
    match sub {
        [] => list_skills(dir),
        [a] if a == "add" => {
            Err("error: bit skills add needs a repo. hint: bit skills add owner/repo".into())
        }
        [a, repo] if a == "add" => add(dir, repo),
        [a] if a == "remove" => Err("error: bit skills remove needs a name".into()),
        [a, name] if a == "remove" => remove(dir, name),
        [a] if a == "update" => update_all(dir),
        [a, name] if a == "update" => update_one(dir, name),
        _ => Err("error: unknown bit skills command. hint: bit skills add|remove|update".into()),
    }
}

fn list_skills(dir: &Path) -> Result<(), String> {
    let skills = list(dir);
    if skills.is_empty() {
        println!("no skills in {}", dir.display());
        return Ok(());
    }
    for k in skills {
        println!("{:<20} {}", k.name, k.description);
    }
    Ok(())
}

fn add(dir: &Path, repo: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("error: {}: {e}", dir.display()))?;
    let url = normalize_repo(repo);
    let name = skill_name(&url);
    let path = dir.join(&name);
    if path.exists() {
        return Err(format!(
            "error: skill `{name}` already exists. hint: bit skills update {name}"
        ));
    }
    let out = Command::new("git")
        .args(["clone", "--depth", "1", &url, path.to_str().unwrap_or(&name)])
        .output()
        .map_err(|e| format!("error: git clone failed: {e}. {FIX}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&path);
        return Err(format!("error: git clone failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let md = path.join("SKILL.md");
    let text = std::fs::read_to_string(&md)
        .map_err(|e| format!("error: {}: {e}. hint: a skill needs a SKILL.md", md.display()))?;
    let (n, description) = agent::skills::parse(&text).ok_or_else(|| {
        format!("error: {} is missing a name/description frontmatter", md.display())
    })?;
    if n != name {
        // The directory name is the handle; allow the frontmatter name to differ, but warn.
        eprintln!("bit: warning: skill folder is `{name}` but frontmatter names it `{n}`");
    }
    println!("added {name}: {description}");
    Ok(())
}

fn remove(dir: &Path, name: &str) -> Result<(), String> {
    let path = dir.join(name);
    std::fs::remove_dir_all(&path).map_err(|e| format!("error: {}: {e}", path.display()))?;
    println!("removed {name}");
    Ok(())
}

fn update_all(dir: &Path) -> Result<(), String> {
    let skills = list(dir);
    if skills.is_empty() {
        println!("no skills to update");
        return Ok(());
    }
    let mut failed = false;
    for k in skills {
        if let Err(e) = git_pull(k.path.parent().unwrap_or(dir)) {
            eprintln!("{e}");
            failed = true;
        }
    }
    if failed { Err("error: one or more skills failed to update".into()) } else { Ok(()) }
}

fn update_one(dir: &Path, name: &str) -> Result<(), String> {
    git_pull(&dir.join(name))
}

fn git_pull(path: &Path) -> Result<(), String> {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
    let out = Command::new("git")
        .arg("pull")
        .current_dir(path)
        .output()
        .map_err(|e| format!("error: git pull failed for {name}: {e}. {FIX}"))?;
    if out.status.success() {
        println!("updated {name}: {}", String::from_utf8_lossy(&out.stdout).trim());
        Ok(())
    } else {
        Err(format!("error: git pull failed for {name}: {}", String::from_utf8_lossy(&out.stderr)))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_short_repo() {
        assert_eq!(normalize_repo("owner/repo"), "https://github.com/owner/repo.git");
        assert_eq!(skill_name("https://github.com/owner/repo.git"), "repo");
    }
}
