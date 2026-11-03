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
    pub remember: Option<PathBuf>,
    /// Bash commands that run without asking.
    pub allow: Vec<String>,
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

/// (name, base_url, api key env var).
const BUILTIN: &[(&str, &str, &str)] = &[
    ("deepseek", "https://api.deepseek.com/v1", "DEEPSEEK_API_KEY"),
    ("openai", "https://api.openai.com/v1", "OPENAI_API_KEY"),
    // https://opencode.ai/v2/docs/console/go/; only its Chat Completions models work.
    ("opencode-go", "https://opencode.ai/inference/go/openai/v1", "OPENCODE_API_KEY"),
];

pub fn load() -> Result<Config, String> {
    let path = dir("XDG_CONFIG_HOME", ".config")
        .map(|d| d.join("bit/config.toml"))
        .ok_or("error: cannot find the config directory. hint: set HOME")?;
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
    let mut providers = Vec::new();
    let names = BUILTIN.iter().map(|b| b.0.to_owned());
    let extra: Vec<String> =
        file.providers.keys().filter(|n| !BUILTIN.iter().any(|b| b.0 == *n)).cloned().collect();
    for name in names.chain(extra) {
        let builtin = BUILTIN.iter().find(|b| b.0 == name);
        let conf = file.providers.remove(&name);
        let pick = |c: Option<String>, b: Option<&str>, what: &str| {
            c.or(b.map(Into::into)).ok_or_else(|| {
                format!("error: [providers.{name}] needs {what}. hint: add it in {at}")
            })
        };
        let (c_url, c_env, c_models) =
            conf.map_or((None, None, Vec::new()), |c| (c.base_url, c.api_key_env, c.models));
        let base_url = pick(c_url, builtin.map(|b| b.1), "base_url")?;
        let key_env = pick(c_env, builtin.map(|b| b.2), "api_key_env")?;
        let key = std::env::var(&key_env).map_err(|_| {
            format!("error: {key_env} is not set. hint: export it, or set api_key_env for {name} in {at}")
        });
        let mut models = c_models;
        let built: Vec<String> =
            agent::models::list(&name).map(String::from).filter(|m| !models.contains(m)).collect();
        models.extend(built);
        providers.push(agent::Provider { name, base_url, key_env, key, models });
    }
    providers.sort_by_key(|p| p.key.is_err());

    let detected = providers.iter().find(|p| p.key.is_ok()).map_or("deepseek", |p| &p.name);
    let name = file.provider.clone().unwrap_or_else(|| detected.into());
    if !providers.iter().any(|p| p.name == name) {
        return Err(format!(
            "error: unknown provider `{name}`. hint: add [providers.{name}] to {at}"
        ));
    }
    let remember = dir("XDG_STATE_HOME", ".local/state").map(|d| d.join("bit/model"));
    let last = remember.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| {
        let (p, m) = s.trim().split_once('/')?;
        let known = providers.iter().any(|x| x.name == p) && !m.is_empty();
        known.then(|| (p.to_owned(), m.to_owned()))
    });
    Ok(Config {
        providers,
        configured: (name, file.model.unwrap_or_default()),
        last,
        remember,
        allow: file.bash.allow,
    })
}

pub fn sessions_dir() -> Result<PathBuf, String> {
    let base = dir("XDG_DATA_HOME", ".local/share")
        .ok_or("error: cannot find the data directory. hint: set HOME")?;
    Ok(base.join("bit/sessions"))
}

/// `$var`, else `$HOME/<default>`.
fn dir(var: &str, default: &str) -> Option<PathBuf> {
    let home = || std::env::var_os("HOME").map(|h| PathBuf::from(h).join(default));
    std::env::var_os(var).map(PathBuf::from).or_else(home)
}
