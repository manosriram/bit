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
        [a, name] if a == "update" => git_pull(&dir.join(name)),
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
    let msg = agent::skills::add(dir, repo).map_err(|e| format!("error: {e}"))?;
    println!("{msg}");
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

fn git_pull(path: &Path) -> Result<(), String> {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
    if path.join(".git").is_dir() {
        return pull_git(path, name);
    }
    if path.join(".source").is_file() {
        return pull_source(path, name);
    }
    Err(format!("error: `{name}` is not a git repo and has no .source file; cannot update"))
}

fn pull_git(path: &Path, name: &str) -> Result<(), String> {
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

fn pull_source(path: &Path, name: &str) -> Result<(), String> {
    let source = std::fs::read_to_string(path.join(".source"))
        .map_err(|e| format!("error: could not read .source for {name}: {e}"))?;
    let mut lines = source.lines();
    let url = lines.next().filter(|u| !u.is_empty());
    let url = url.ok_or_else(|| format!("error: .source for {name} has no url"))?;
    let subpath = lines.next().unwrap_or("");

    let tmp = path.parent().unwrap_or(Path::new(".")).join(format!(".tmp-update-{name}"));
    let _ = std::fs::remove_dir_all(&tmp);
    let out = Command::new("git")
        .args(["clone", "--depth", "1", url])
        .arg(&tmp)
        .output()
        .map_err(|e| format!("error: git clone failed for {name}: {e}. {FIX}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!(
            "error: git clone failed for {name}: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    let src = tmp.join(subpath);
    if !src.is_dir() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!("error: source path `{}` not found for {name}", src.display()));
    }

    for e in std::fs::read_dir(path).map_err(|e| format!("error: {}: {e}", path.display()))? {
        let e = e.map_err(|e| format!("error: {}: {e}", path.display()))?;
        let p = e.path();
        if p.file_name().is_some_and(|n| n == ".source") {
            continue;
        }
        if p.is_dir() {
            std::fs::remove_dir_all(&p).map_err(|e| format!("error: {}: {e}", p.display()))?;
        } else {
            std::fs::remove_file(&p).map_err(|e| format!("error: {}: {e}", p.display()))?;
        }
    }

    for e in std::fs::read_dir(&src).map_err(|e| format!("error: {}: {e}", src.display()))? {
        let e = e.map_err(|e| format!("error: {}: {e}", src.display()))?;
        let from = e.path();
        let to = path.join(from.file_name().unwrap_or_default());
        std::fs::rename(&from, &to)
            .map_err(|e| format!("error: {} -> {}: {e}", from.display(), to.display()))?;
    }

    let _ = std::fs::remove_dir_all(&tmp);
    println!("updated {name}");
    Ok(())
}
