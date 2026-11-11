use agent::providers::{self, Auth, auth::Cred};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub struct Config {
    /// Every provider: the built-in ones (with config overrides) then the config's own.
    /// Those with a key set come first.
    pub providers: Vec<agent::Provider>,
    /// From the config's `provider` and `model`, else the first provider with a key. The
    /// model is empty when the config names none.
    pub configured: (String, String),
    /// The last pick, as (provider, model), if its provider still exists.
    pub last: Option<(String, String)>,
    /// Where the last pick is kept.
    pub remember: PathBuf,
    /// The last thinking level picked, kept beside it.
    pub thinking: Option<String>,
    /// `auth.json`: saved API keys and the ChatGPT sign-in.
    pub auth: PathBuf,
    /// Where the session logs go.
    pub sessions: PathBuf,
    /// Bash commands that run without asking.
    pub allow: Vec<String>,
    /// Where the Lua plugins are.
    pub plugins: PathBuf,
    /// Where the skills are.
    pub skills: PathBuf,
}

/// Unknown keys land in `unknown` so they can be warned about instead of failing the load.
#[derive(Deserialize, Default)]
struct File {
    model: Option<String>,
    provider: Option<String>,
    #[serde(default)]
    providers: BTreeMap<String, Provider>,
    #[serde(default)]
    bash: Bash,
    #[serde(flatten)]
    unknown: toml::Table,
}

#[derive(Deserialize, Default)]
struct Bash {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(flatten)]
    unknown: toml::Table,
}

/// A built-in provider needs neither field; a new one needs both.
#[derive(Deserialize)]
struct Provider {
    base_url: Option<String>,
    api_key_env: Option<String>,
    #[serde(default)]
    models: Vec<String>,
    #[serde(flatten)]
    unknown: toml::Table,
}

pub fn load() -> Result<Config, String> {
    // Everything bit keeps lives here: config, credentials, the last pick, sessions.
    let home = || std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"));
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(home);
    let base = base.ok_or("error: cannot find the config directory. hint: set HOME")?.join("bit");
    let (path, auth) = (base.join("config.toml"), base.join("auth.json"));
    let mut file: File = match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).map_err(|e| format!("error: {}: {e}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => File::default(),
        Err(e) => return Err(format!("error: {}: {e}", path.display())),
    };
    let unknown = file.unknown.keys().map(String::as_str);
    let nested = file
        .providers
        .iter()
        .flat_map(|(n, p)| p.unknown.keys().map(move |k| format!("providers.{n}.{k}")))
        .chain(file.bash.unknown.keys().map(|k| format!("bash.{k}")));
    for key in unknown.map(str::to_owned).chain(nested) {
        eprintln!("bit: warning: unknown config key `{key}` in {}", path.display());
    }

    let at = path.display();
    let mut saved = providers::auth::load(&auth)?;
    let mut providers = Vec::new();
    let names = providers::ALL.iter().map(|s| s.name.to_owned());
    let extra: Vec<String> =
        file.providers.keys().filter(|n| providers::find(n).is_none()).cloned().collect();
    for name in names.chain(extra) {
        let builtin = providers::find(&name);
        let conf = file.providers.remove(&name);
        let pick = |c: Option<String>, b: Option<&str>, what: &str| {
            c.or(b.map(Into::into)).ok_or_else(|| {
                format!("error: [providers.{name}] needs {what}. hint: add it in {at}")
            })
        };
        let (c_url, c_env, c_models) =
            conf.map_or((None, None, Vec::new()), |c| (c.base_url, c.api_key_env, c.models));
        let base_url = pick(c_url, builtin.map(|s| s.base_url), "base_url")?;
        let key_env = pick(c_env, builtin.map(|s| s.key_env()), "api_key_env")?;
        let key = match builtin.map(|s| (s, &s.auth)) {
            Some((s, Auth::Oauth)) => signed_in(s, &auth),
            _ => api_key(&name, &key_env, std::env::var(&key_env).ok(), saved.remove(&name)),
        };
        let mut models = c_models;
        let built: Vec<String> =
            agent::models::list(&name).map(String::from).filter(|m| !models.contains(m)).collect();
        models.extend(built);
        providers.push(agent::Provider { name, base_url, key_env, key, models });
    }
    providers.sort_by_key(|p| p.key.is_err());

    let detected =
        providers.iter().find(|p| p.key.is_ok()).map_or(providers::ALL[0].name, |p| &p.name);
    let name = file.provider.clone().unwrap_or_else(|| detected.into());
    if !providers.iter().any(|p| p.name == name) {
        return Err(format!(
            "error: unknown provider `{name}`. hint: add [providers.{name}] to {at}"
        ));
    }
    let remember = base.join("model");
    let last = std::fs::read_to_string(&remember).ok().and_then(|s| {
        let (p, m) = s.trim().split_once('/')?;
        let known = providers.iter().any(|x| x.name == p) && !m.is_empty();
        known.then(|| (p.to_owned(), m.to_owned()))
    });
    let thinking = std::fs::read_to_string(base.join("thinking")).ok();
    Ok(Config {
        thinking: thinking.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()),
        providers,
        configured: (name, file.model.unwrap_or_default()),
        last,
        remember,
        auth,
        sessions: base.join("sessions"),
        allow: file.bash.allow,
        plugins: base.join("plugins"),
        skills: base.join("skills"),
    })
}

/// The environment first, so a shell can override a saved key; then `auth.json`.
fn api_key(
    name: &str,
    key_env: &str,
    env: Option<String>,
    saved: Option<Cred>,
) -> Result<String, String> {
    match (env, saved) {
        (Some(k), _) | (None, Some(Cred::ApiKey { key: k })) => Ok(k),
        _ => Err(format!(
            "error: {key_env} is not set. hint: run /login to add your {name} key, or export it"
        )),
    }
}

/// The saved sign-in. Its own runtime, since a refresh is a request and the session's
/// runtime starts later.
fn signed_in(s: &providers::Spec, auth: &std::path::Path) -> Result<String, String> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build();
    rt.map_err(|e| e.to_string())?.block_on(providers::token(s, auth))
}

#[cfg(test)]
mod tests {
    use agent::providers::auth::Cred;

    #[test]
    fn the_environment_wins_over_a_saved_key() {
        let key = |env: Option<&str>, saved: Option<&str>| {
            let saved = saved.map(|k| Cred::ApiKey { key: k.into() });
            super::api_key("deepseek", "DEEPSEEK_API_KEY", env.map(Into::into), saved)
        };
        assert_eq!(key(Some("env"), Some("file")), Ok("env".into()));
        assert_eq!(key(None, Some("file")), Ok("file".into()));
        assert_eq!(
            key(None, None),
            Err("error: DEEPSEEK_API_KEY is not set. hint: run /login to add your deepseek key, or export it".into())
        );
    }
}
