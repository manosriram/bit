//! The built-in providers. Each is one `Spec`: an API-key one is a row in `keyed.rs`, a
//! signed-in one a file of its own with its OAuth flow. Adding or removing one is that row
//! or file, its entry in `ALL`, and (for a sign-in) its arm in `sign_in` and `token`.

pub mod auth;
mod chatgpt;
mod keyed;

use std::path::Path;

pub struct Spec {
    pub name: &'static str,
    pub base_url: &'static str,
    pub wire: llm::Api,
    pub auth: Auth,
    /// Send the session id as `prompt_cache_key`, for providers that route their cache by it.
    pub cache_key: bool,
    /// Headers besides the user agent, from the session id and the key.
    pub headers: fn(session: &str, key: &str) -> Vec<(&'static str, String)>,
}

pub enum Auth {
    /// The variable that holds the key.
    Key(&'static str),
    /// Signed in with `/login`, under this choice.
    Oauth(&'static str),
}

impl Spec {
    /// What the picker shows for the key: its variable, or `/login`.
    pub fn key_env(&self) -> &'static str {
        match self.auth {
            Auth::Key(env) => env,
            Auth::Oauth(_) => "/login",
        }
    }
}

/// In the order the picker lists them; the first is the default without any key.
pub const ALL: &[&Spec] = &[&keyed::DEEPSEEK, &keyed::OPENAI, &chatgpt::SPEC, &keyed::OPENCODE_GO];

pub fn find(name: &str) -> Option<&'static Spec> {
    ALL.iter().copied().find(|s| s.name == name)
}

/// Signs in to `s` (its URL goes to `show` too) and gives the access token.
pub async fn sign_in(s: &Spec, auth: &Path, show: impl FnOnce(&str)) -> Result<String, String> {
    match s.name {
        "chatgpt" => chatgpt::sign_in(auth, show).await,
        n => Err(format!("error: {n} has no sign-in")),
    }
}

/// The saved access token for `s`, refreshed when it's about to expire.
pub async fn token(s: &Spec, auth: &Path) -> Result<String, String> {
    match s.name {
        "chatgpt" => chatgpt::token(auth).await,
        n => Err(format!("error: {n} has no sign-in")),
    }
}

#[cfg(test)]
mod tests {
    /// Provider names live here and in the model list only, so a provider comes and goes
    /// with its own files.
    #[test]
    fn no_provider_name_is_written_outside_providers() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
        let mut files = vec![std::path::PathBuf::from(root)];
        let mut found = Vec::new();
        while let Some(p) = files.pop() {
            if p.is_dir() {
                files.extend(std::fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
                continue;
            }
            let path = p.to_string_lossy();
            if !path.ends_with(".rs") || path.contains("/providers/") || path.ends_with("models.rs")
            {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            let code = text.split("#[cfg(test)]").next().unwrap();
            for s in super::ALL {
                if code.contains(&format!("\"{}\"", s.name)) {
                    found.push(format!("{path}: {}", s.name));
                }
            }
        }
        assert!(found.is_empty(), "{found:?}");
    }
}
