//! `auth.json`, the saved credentials (one entry per provider), and "Sign in with ChatGPT"
//! for the `chatgpt` provider: the OAuth flow Codex CLI uses (PKCE, with the browser sent
//! back to localhost:1455). bit keeps its own tokens, since refresh tokens rotate and
//! sharing Codex's file would sign one of the two out.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const ISSUER: &str = "https://auth.openai.com";
const REDIRECT: &str = "http://localhost:1455/auth/callback";
/// Access tokens last ten days; refreshing a day early keeps one from expiring mid-session.
const EARLY: u64 = 24 * 3600;
const AGAIN: &str = "hint: run /login";

/// One provider's entry in `auth.json`, by provider name.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Cred {
    ApiKey { key: String },
    Oauth(Tokens),
}

/// A sign-in, as saved and as a token reply: a refresh may leave out the refresh token.
#[derive(Serialize, Deserialize)]
pub struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
}

/// The saved credentials; none when the file doesn't exist yet.
pub fn load(path: &Path) -> Result<BTreeMap<String, Cred>, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("error: {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(format!("error: {}: {e}", path.display())),
    }
}

/// Opens the sign-in page (its URL goes to `show` too, in case no browser opens), waits for
/// the browser to come back, and saves the tokens in `path`. Gives the access token.
pub async fn sign_in(path: &Path, show: impl FnOnce(&str)) -> Result<String, String> {
    let mut seed = [0; 48];
    aws_lc_rs::rand::fill(&mut seed).map_err(|_| "error: the system RNG failed")?;
    let (verifier, state) = (B64.encode(&seed[..32]), B64.encode(&seed[32..]));
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, verifier.as_bytes());
    let query = [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT),
        ("scope", "openid profile email offline_access"),
        ("code_challenge", &B64.encode(digest)),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", &state),
        ("originator", "bit"),
    ];
    let url = format!("{ISSUER}/oauth/authorize?{}", form(&query));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:1455").await.map_err(|e| {
        format!("error: cannot listen on localhost:1455: {e}. hint: close the other sign-in (Codex uses the same port)")
    })?;
    show(&url);
    let open = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    // tokio reaps the child once it exits, so a browser that stays open leaves no zombie.
    let quiet = std::process::Stdio::null;
    let _ = tokio::process::Command::new(open).arg(&url).stdout(quiet()).stderr(quiet()).spawn();
    let code = loop {
        let (mut conn, _) = listener.accept().await.map_err(|e| format!("error: sign-in: {e}"))?;
        let mut line = String::new();
        let _ = BufReader::new(&mut conn).read_line(&mut line).await;
        let target = line.split(' ').nth(1).unwrap_or("");
        // Anything else (a favicon) gets the connection closed.
        let Some(q) = target.strip_prefix("/auth/callback?") else { continue };
        let param = |k: &str| q.split('&').find_map(|p| p.strip_prefix(k)?.strip_prefix('='));
        let r = match param("code").filter(|_| param("state") == Some(&state)) {
            Some(code) => Ok(code.to_owned()),
            None => Err(format!(
                "error: sign-in failed: {}. {AGAIN}",
                param("error").unwrap_or("no code")
            )),
        };
        let page =
            if r.is_ok() { "Signed in to bit. You can close this tab." } else { "Sign-in failed." };
        let res = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        let _ = conn.write_all(res.as_bytes()).await;
        break r?;
    };
    let grant = [
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", REDIRECT),
        ("client_id", CLIENT_ID),
        ("code_verifier", &verifier),
    ];
    let t = exchange(&grant).await?;
    let (access, mut creds) = (t.access_token.clone(), load(path)?);
    creds.insert("chatgpt".into(), Cred::Oauth(t));
    save(path, &creds)?;
    Ok(access)
}

/// The saved access token, refreshed first when it's about to expire.
pub async fn token(path: &Path) -> Result<String, String> {
    let mut creds = load(path)?;
    let Some(Cred::Oauth(t)) = creds.get_mut("chatgpt") else {
        return Err(format!("error: not signed in to ChatGPT. {AGAIN}"));
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    if claims(&t.access_token)["exp"].as_u64().unwrap_or(0) < now.map_or(0, |d| d.as_secs()) + EARLY
    {
        let grant = [
            ("grant_type", "refresh_token"),
            ("refresh_token", t.refresh_token.as_deref().unwrap_or("")),
            ("client_id", CLIENT_ID),
            ("scope", "openid profile email"),
        ];
        let r = exchange(&grant).await.map_err(|e| format!("{e}. {AGAIN}"))?;
        t.refresh_token = r.refresh_token.or(t.refresh_token.take());
        t.access_token = r.access_token;
        let access = t.access_token.clone();
        save(path, &creds)?;
        return Ok(access);
    }
    Ok(t.access_token.clone())
}

/// The ChatGPT account id from the token's claims, which the backend wants as a header.
pub(crate) fn account(token: &str) -> String {
    claims(token)["https://api.openai.com/auth"]["chatgpt_account_id"].as_str().unwrap_or("").into()
}

/// The JWT's payload; the signature is the server's to check.
fn claims(jwt: &str) -> serde_json::Value {
    let payload = jwt.split('.').nth(1).and_then(|p| B64.decode(p).ok());
    payload.and_then(|p| serde_json::from_slice(&p).ok()).unwrap_or_default()
}

async fn exchange(fields: &[(&str, &str)]) -> Result<Tokens, String> {
    let fail = |e: &dyn std::fmt::Display| format!("error: ChatGPT sign-in: {e}");
    let res = reqwest::Client::new()
        .post(format!("{ISSUER}/oauth/token"))
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .timeout(std::time::Duration::from_secs(15))
        .body(form(fields))
        .send()
        .await
        .map_err(|e| fail(&e))?;
    let status = res.status();
    let body = res.text().await.map_err(|e| fail(&e))?;
    if !status.is_success() {
        return Err(fail(&format!("status {status}: {body}")));
    }
    serde_json::from_str(&body).map_err(|e| fail(&e))
}

/// Owner-only, since the keys and tokens are as good as a password.
fn save(path: &Path, creds: &BTreeMap<String, Cred>) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let fail = |e: std::io::Error| format!("error: {}: {e}", path.display());
    std::fs::create_dir_all(path.parent().unwrap_or(path)).map_err(fail)?;
    let json = serde_json::to_string_pretty(creds).unwrap_or_default();
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true).mode(0o600);
    o.open(path).and_then(|mut f| f.write_all(json.as_bytes())).map_err(fail)
}

/// `application/x-www-form-urlencoded`, which is also a valid query string.
fn form(fields: &[(&str, &str)]) -> String {
    let safe = |b: &u8| b.is_ascii_alphanumeric() || b"-._~".contains(b);
    let enc = |s: &str| -> String {
        s.bytes().map(|b| if safe(&b) { (b as char).into() } else { format!("%{b:02X}") }).collect()
    };
    let pairs = fields.iter().map(|(k, v)| format!("{k}={}", enc(v)));
    pairs.collect::<Vec<_>>().join("&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("bit-login-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// A token that expires in `secs`, for account `acc-1`. Unsigned: bit never checks.
    fn jwt(secs: u64) -> String {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
        let claims = serde_json::json!({"exp": now.as_secs() + secs, "https://api.openai.com/auth": {"chatgpt_account_id": "acc-1"}});
        format!("h.{}.s", B64.encode(claims.to_string()))
    }

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

    #[tokio::test]
    async fn a_fresh_token_is_used_as_is_and_other_entries_stay() {
        let path = dir("fresh").join("auth.json");
        let access = jwt(5 * 24 * 3600);
        let mut creds = BTreeMap::new();
        creds.insert("deepseek".into(), Cred::ApiKey { key: "sk-1".into() });
        assert!(token(&path).await.unwrap_err().contains("not signed in"));
        let t = Tokens { access_token: access.clone(), refresh_token: Some("r".into()) };
        creds.insert("chatgpt".into(), Cred::Oauth(t));
        save(&path, &creds).unwrap();
        // More than a day left, so no refresh request is made.
        assert_eq!(token(&path).await.unwrap(), access);
        assert_eq!(account(&access), "acc-1");
        assert!(matches!(&load(&path).unwrap()["deepseek"], Cred::ApiKey { key } if key == "sk-1"));
    }

    #[test]
    fn form_percent_encodes_all_but_unreserved_bytes() {
        let f = form(&[("a", "x y/z:é"), ("b", "AZaz09-._~")]);
        assert_eq!(f, "a=x%20y%2Fz%3A%C3%A9&b=AZaz09-._~");
        assert_eq!(account("not-a-jwt"), "");
    }
}
