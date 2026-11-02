use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub struct Config {
    pub provider: String,
    /// `None` until the user picks one.
    pub model: Option<String>,
    pub base_url: String,
    pub api_key: String,
    pub headers: Vec<(Box<str>, Box<str>)>,
    /// Bash commands that run without asking.
    pub allow: Vec<String>,
    pub prices: BTreeMap<String, agent::Price>,
    /// What the model picker offers: config `[models]` entries, then the built-in ones.
    pub models: Vec<String>,
    pub max_cost: Option<f64>,
    /// Shown in the TUI at startup, e.g. a missing API key.
    pub notices: Vec<String>,
}

/// Unknown keys land in `unknown` so they can be warned about instead of failing the load.
#[derive(Deserialize, Default)]
struct File {
    model: Option<String>,
    provider: Option<String>,
    max_cost: Option<f64>,
    #[serde(default)]
    models: BTreeMap<String, agent::Price>,
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

#[derive(Deserialize)]
struct Provider {
    base_url: String,
    api_key_env: String,
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

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("bit/config.toml"))
}

pub fn load(model: Option<String>) -> Result<Config, String> {
    let path = path().ok_or("error: cannot find the config directory. hint: set HOME")?;
    let file: File = match std::fs::read_to_string(&path) {
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

    let name = file.provider.as_deref().unwrap_or_else(|| {
        BUILTIN.iter().find(|b| std::env::var_os(b.2).is_some()).map_or("deepseek", |b| b.0)
    });
    let builtin = BUILTIN.iter().find(|b| b.0 == name);
    let (base_url, key_env) = match file.providers.get(name) {
        Some(p) => (p.base_url.clone(), p.api_key_env.clone()),
        None => builtin.map(|b| (b.1.to_owned(), b.2.to_owned())).ok_or_else(|| {
            format!(
                "error: unknown provider `{name}`. hint: add [providers.{name}] to {}",
                path.display()
            )
        })?,
    };
    let mut notices = Vec::new();
    let api_key = std::env::var(&key_env).unwrap_or_else(|_| {
        notices.push(format!(
            "error: {key_env} is not set. hint: export it, or set api_key_env for {name} in {}",
            path.display()
        ));
        String::new()
    });
    let mut models: Vec<String> = file.models.keys().cloned().collect();
    models.extend(
        agent::models::list(name).filter(|m| !file.models.contains_key(*m)).map(Into::into),
    );
    let headers = vec![("user-agent".into(), concat!("bit/", env!("CARGO_PKG_VERSION")).into())];
    Ok(Config {
        provider: name.into(),
        model: model.or(file.model),
        base_url,
        api_key,
        headers,
        allow: file.bash.allow,
        prices: file.models,
        models,
        max_cost: file.max_cost,
        notices,
    })
}

pub fn sessions_dir() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or("error: cannot find the data directory. hint: set HOME")?;
    Ok(base.join("bit/sessions"))
}
