//! GOG authentication (same public Galaxy client credentials used by Heroic and lgogdownloader).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const CLIENT_ID: &str = "46899977096215655";
const CLIENT_SECRET: &str = "9d85c43b1482497dbbce61f6e4aa173a433796eeae2ca8c5f6129f2dc4de46d9";
const REDIRECT_URI: &str = "https://embed.gog.com/on_login_success?origin=client";

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Could not find a login code in the pasted text")]
    NoCode,
    #[error("Login window was closed")]
    Cancelled,
    #[error("Could not open the login window: {0}")]
    Spawn(String),
    #[error("Network error: {0}")]
    Net(#[from] reqwest::Error),
    #[error("GOG rejected the login: {0}")]
    Rejected(String),
    #[error("Could not save session: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    pub user_id: String,
    pub username: String,
    /// Unix seconds
    pub expires_at: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    user_id: String,
    expires_in: u64,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn login_url() -> String {
    format!(
        "https://auth.gog.com/auth?client_id={CLIENT_ID}&redirect_uri=https%3A%2F%2Fembed.gog.com%2Fon_login_success%3Forigin%3Dclient&response_type=code&layout=galaxy"
    )
}

/// Accepts either the full redirect URL or the bare code.
pub fn extract_code(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(i) = s.find("code=") {
        let rest = &s[i + 5..];
        let end = rest.find('&').unwrap_or(rest.len());
        let code = &rest[..end];
        return if code.is_empty() { None } else { Some(code.to_string()) };
    }
    if s.contains('/') || s.contains('?') || s.contains(' ') {
        return None;
    }
    Some(s.to_string())
}

async fn token_request(params: &[(&str, &str)]) -> Result<TokenResponse, AuthError> {
    let resp = reqwest::Client::new()
        .get("https://auth.gog.com/token")
        .query(&[("client_id", CLIENT_ID), ("client_secret", CLIENT_SECRET)])
        .query(params)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(AuthError::Rejected(format!("HTTP {}", resp.status())));
    }
    Ok(resp.json().await?)
}

async fn fetch_username(token: &str) -> String {
    let v: Option<serde_json::Value> = async {
        reqwest::Client::new()
            .get("https://embed.gog.com/userData.json")
            .bearer_auth(token)
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()
    }
    .await;
    v.and_then(|v| v["username"].as_str().map(String::from))
        .unwrap_or_else(|| "GOG user".into())
}

/// Fetch the account avatar without delaying window creation or library rendering.
pub async fn avatar_url(token: &str) -> Option<String> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build().ok()?;
    let data: serde_json::Value = client.get("https://embed.gog.com/userData.json")
        .bearer_auth(token).send().await.ok()?.error_for_status().ok()?.json().await.ok()?;
    let result = [data["avatar"].as_str(), data["avatar"]["large"].as_str(), data["avatar"]["big"].as_str(),
     data["avatar"]["links"]["large"]["href"].as_str()]
        .into_iter().flatten().find(|url| url.starts_with("https://") || url.starts_with("//"))
        .map(str::to_owned);
    result
}

/// Opens the embedded GOG login window (child process) and completes the OAuth exchange.
/// `creds` pre-fills the GOG form; `provider` ("google", "steam", "xbox") picks a social login.
pub async fn login_via_webview(
    provider: &str,
    creds: Option<(String, String)>,
) -> Result<Session, AuthError> {
    use tokio::io::AsyncWriteExt;
    let exe = std::env::current_exe().map_err(|e| AuthError::Spawn(e.to_string()))?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("--login-webview").arg("--provider").arg(provider);
    if creds.is_some() {
        cmd.arg("--creds");
    }
    cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| AuthError::Spawn(e.to_string()))?;
    if let Some(mut stdin) = child.stdin.take() {
        if let Some((e, p)) = creds {
            let _ = stdin.write_all(format!("{e}\n{p}\n").as_bytes()).await;
        }
    }
    let out = child.wait_with_output().await.map_err(|e| AuthError::Spawn(e.to_string()))?;
    if !out.status.success() {
        return Err(AuthError::Cancelled);
    }
    login_with_code(String::from_utf8_lossy(&out.stdout).trim()).await
}

pub async fn login_with_code(input: &str) -> Result<Session, AuthError> {
    let code = extract_code(input).ok_or(AuthError::NoCode)?;
    let t = token_request(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", REDIRECT_URI),
    ])
    .await?;
    let username = fetch_username(&t.access_token).await;
    let s = Session {
        access_token: t.access_token,
        refresh_token: t.refresh_token,
        user_id: t.user_id,
        username,
        expires_at: now() + t.expires_in,
    };
    s.save()?;
    Ok(s)
}

impl Session {
    fn path() -> PathBuf {
        crate::library::migrate_dir(dirs::config_dir()).join("session.json")
    }

    pub fn load() -> Option<Session> {
        serde_json::from_slice(&std::fs::read(Self::path()).ok()?).ok()
    }

    pub fn save(&self) -> Result<(), AuthError> {
        let p = Self::path();
        std::fs::create_dir_all(p.parent().unwrap())?;
        std::fs::write(&p, serde_json::to_vec_pretty(self).unwrap())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn delete() {
        let _ = std::fs::remove_file(Self::path());
    }

    pub fn is_expired(&self) -> bool {
        now() + 60 >= self.expires_at
    }

    pub async fn ensure_fresh(self) -> Result<Session, AuthError> {
        if self.is_expired() {
            self.refresh().await
        } else {
            Ok(self)
        }
    }

    pub async fn refresh(&self) -> Result<Session, AuthError> {
        let t = token_request(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &self.refresh_token),
        ])
        .await?;
        let s = Session {
            access_token: t.access_token,
            refresh_token: t.refresh_token,
            user_id: t.user_id,
            username: self.username.clone(),
            expires_at: now() + t.expires_in,
        };
        s.save()?;
        Ok(s)
    }
}

pub fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_code_from_url() {
        let u = "https://embed.gog.com/on_login_success?origin=client&code=abc123XYZ";
        assert_eq!(extract_code(u).as_deref(), Some("abc123XYZ"));
        let u2 = "https://embed.gog.com/on_login_success?code=abc&origin=client";
        assert_eq!(extract_code(u2).as_deref(), Some("abc"));
    }

    #[test]
    fn accepts_bare_code_rejects_junk() {
        assert_eq!(extract_code("  abc123 ").as_deref(), Some("abc123"));
        assert_eq!(extract_code(""), None);
        assert_eq!(extract_code("https://example.com/x"), None);
    }
}
