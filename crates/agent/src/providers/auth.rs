//! `auth.json`: the saved credentials, one entry per provider name.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// One provider's entry in `auth.json`.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Cred {
    ApiKey { key: String },
    Oauth(Tokens),
}

/// A sign-in, as saved and as a token reply: a refresh may leave out the refresh token.
#[derive(Serialize, Deserialize)]
pub struct Tokens {
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
}

/// The saved credentials; none when the file doesn't exist yet.
pub fn load(path: &Path) -> Result<BTreeMap<String, Cred>, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("error: {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(format!("error: {}: {e}", path.display())),
    }
}

/// Owner-only, since the keys and tokens are as good as a password.
pub(crate) fn save(path: &Path, creds: &BTreeMap<String, Cred>) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let fail = |e: std::io::Error| format!("error: {}: {e}", path.display());
    std::fs::create_dir_all(path.parent().unwrap_or(path)).map_err(fail)?;
    let json = serde_json::to_string_pretty(creds).unwrap_or_default();
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true).mode(0o600);
    o.open(path).and_then(|mut f| f.write_all(json.as_bytes())).map_err(fail)
}

#[cfg(test)]
pub(crate) fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("bit-auth-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn auth_json_holds_api_keys_and_sign_ins_and_is_saved_owner_only() {
        let path = dir("format").join("auth.json");
        let file = r#"{"deepseek":{"type":"api_key","key":"sk-1"},"chatgpt":{"type":"oauth","access_token":"a","refresh_token":"r"}}"#;
        let creds: BTreeMap<String, Cred> = serde_json::from_str(file).unwrap();
        assert!(matches!(&creds["deepseek"], Cred::ApiKey { key } if key == "sk-1"));
        assert!(matches!(&creds["chatgpt"], Cred::Oauth(t) if t.access_token == "a"));
        save(&path, &creds).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let back = serde_json::to_string(&load(&path).unwrap()).unwrap();
        assert_eq!(
            back,
            r#"{"chatgpt":{"type":"oauth","access_token":"a","refresh_token":"r"},"deepseek":{"type":"api_key","key":"sk-1"}}"#
        );
    }

    #[test]
    fn a_missing_file_is_no_credentials_and_a_bad_one_names_itself() {
        let path = dir("missing").join("auth.json");
        assert!(load(&path).unwrap().is_empty());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"x":{"type":"nope"}}"#).unwrap();
        let e = load(&path).err().unwrap();
        assert!(e.contains("auth.json") && e.contains("unknown variant"), "{e}");
    }
}
