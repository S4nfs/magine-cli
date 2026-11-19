use crate::env::Env;
use crate::errors::{MagineError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT_API_URL: &str = "https://magine.cloud";
const LOCAL_HOSTS: [&str; 4] = ["localhost", "127.0.0.1", "[::1]", "::1"];
const CREDENTIALS: &str = "credentials.json";
const PENDING: &str = "pending-login.json";

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Platform {
    Windows,
    Mac,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::Mac
        } else {
            Platform::Other
        }
    }
}

#[allow(deprecated)]
pub fn home_dir(env: &Env) -> PathBuf {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    env.non_empty(var).map(PathBuf::from).or_else(std::env::home_dir).unwrap_or_else(|| PathBuf::from("."))
}

/// %APPDATA%\magine on Windows, $XDG_CONFIG_HOME/magine or ~/.config/magine elsewhere; MAGINE_CONFIG_DIR overrides.
pub fn config_dir_for(env: &Env, platform: Platform, home: &Path) -> PathBuf {
    if let Some(dir) = env.non_empty("MAGINE_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    if platform == Platform::Windows {
        let base = env.non_empty("APPDATA").map(PathBuf::from).unwrap_or_else(|| home.join("AppData").join("Roaming"));
        return base.join("magine");
    }
    env.non_empty("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config")).join("magine")
}

pub fn config_dir(env: &Env) -> PathBuf {
    config_dir_for(env, Platform::current(), &home_dir(env))
}

/// An API origin the key may be sent to: https, or http only for this computer.
pub fn normalize_api_url(raw: &str) -> Result<String> {
    let u = url::Url::parse(raw.trim()).map_err(|_| MagineError::config(format!("\"{raw}\" is not a URL.")))?;
    if !u.username().is_empty() || u.password().is_some() {
        return Err(MagineError::config("The API URL must not contain a user name or password."));
    }
    let host = u.host_str().unwrap_or("");
    let local = LOCAL_HOSTS.contains(&host);
    if u.scheme() != "https" && !(u.scheme() == "http" && local) {
        return Err(MagineError::config("The API URL must use https:// (http:// is allowed only for localhost)."));
    }
    if host.is_empty() {
        return Err(MagineError::config(format!("\"{raw}\" is not a URL.")));
    }
    Ok(u.origin().ascii_serialization())
}

pub fn is_api_key(s: &str) -> bool {
    s.len() == 67 && s.starts_with("mk_") && s[3..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    pub version: u32,
    pub api_url: String,
    pub api_key: String,
    pub key_id: String,
    pub key_prefix: String,
    pub username: String,
    /// "browser" or "key".
    pub method: String,
    pub expires_at: Option<String>,
    pub saved_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PendingLogin {
    pub api_url: String,
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_at: String,
    pub interval: u64,
}

fn io_err(path: &Path, e: std::io::Error) -> MagineError {
    MagineError::config(format!("Could not write {}: {e}", path.display()))
}

/// Written to a temp file and renamed, readable by this user only.
pub fn write_secret_file<T: Serialize>(dir: &Path, name: &str, data: &T) -> Result<PathBuf> {
    create_private_dir(dir).map_err(|e| io_err(dir, e))?;
    let target = dir.join(name);
    let tmp = dir.join(format!(".{name}.{}.{}.tmp", std::process::id(), crate::util::now_ms()));
    let body = serde_json::to_string_pretty(data).map_err(|e| MagineError::config(e.to_string()))? + "\n";
    let write = || -> std::io::Result<()> {
        let mut f = private_file(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, &target)
    };
    if let Err(e) = write() {
        let _ = fs::remove_file(&tmp);
        return Err(io_err(&target, e));
    }
    restrict(&target);
    Ok(target)
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)
}

#[cfg(unix)]
fn private_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_: &Path) {}

fn read_json(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    restrict(path);
    let damaged = || MagineError::config(format!("{} is damaged. Delete it and run \"magine login\" again.", path.display()));
    let text = fs::read_to_string(path).map_err(|_| damaged())?;
    serde_json::from_str(&text).map(Some).map_err(|_| damaged())
}

pub fn load_credentials(dir: &Path) -> Result<Option<Credentials>> {
    let Some(c) = read_json(&dir.join(CREDENTIALS))? else { return Ok(None) };
    let (Some(api_key), Some(api_url)) = (c["apiKey"].as_str(), c["apiUrl"].as_str()) else { return Ok(None) };
    let s = |k: &str| c[k].as_str().unwrap_or("").to_string();
    Ok(Some(Credentials {
        version: 1,
        api_url: api_url.to_string(),
        api_key: api_key.to_string(),
        key_id: s("keyId"),
        key_prefix: c["keyPrefix"].as_str().map(str::to_string).unwrap_or_else(|| format!("{}...", &api_key[..api_key.len().min(11)])),
        username: s("username"),
        method: if c["method"].as_str() == Some("browser") { "browser".into() } else { "key".into() },
        expires_at: c["expiresAt"].as_str().map(str::to_string),
        saved_at: s("savedAt"),
    }))
}

pub fn save_credentials(c: &Credentials, dir: &Path) -> Result<PathBuf> {
    write_secret_file(dir, CREDENTIALS, c)
}

pub fn delete_credentials(dir: &Path) -> bool {
    fs::remove_file(dir.join(CREDENTIALS)).is_ok()
}

pub fn credentials_path(dir: &Path) -> PathBuf {
    dir.join(CREDENTIALS)
}

pub fn load_pending_login(dir: &Path) -> Result<Option<PendingLogin>> {
    let Some(v) = read_json(&dir.join(PENDING))? else { return Ok(None) };
    Ok(serde_json::from_value::<PendingLogin>(v).ok())
}

pub fn save_pending_login(p: &PendingLogin, dir: &Path) -> Result<()> {
    write_secret_file(dir, PENDING, p).map(|_| ())
}

pub fn delete_pending_login(dir: &Path) {
    let _ = fs::remove_file(dir.join(PENDING));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_urls_https_only_http_just_for_this_computer() {
        assert_eq!(normalize_api_url("https://magine.cloud/").unwrap(), "https://magine.cloud");
        assert_eq!(normalize_api_url("https://magine.cloud/some/path?q=1").unwrap(), "https://magine.cloud");
        assert_eq!(normalize_api_url("http://localhost:3000").unwrap(), "http://localhost:3000");
        assert_eq!(normalize_api_url("http://127.0.0.1:9").unwrap(), "http://127.0.0.1:9");
        for bad in ["http://magine.cloud", "https://user:pw@magine.cloud", "ftp://magine.cloud", "not a url"] {
            assert_eq!(normalize_api_url(bad).unwrap_err().exit_code, crate::errors::exit::CONFIG, "{bad}");
        }
    }

    #[test]
    fn config_folder_per_operating_system() {
        let e = Env::from_pairs(&[("MAGINE_CONFIG_DIR", "/x/y")]);
        assert_eq!(config_dir_for(&e, Platform::Other, Path::new("/home/a")), PathBuf::from("/x/y"));
        let e = Env::from_pairs(&[("APPDATA", r"C:\Users\a\AppData\Roaming")]);
        assert_eq!(config_dir_for(&e, Platform::Windows, Path::new(r"C:\Users\a")), Path::new(r"C:\Users\a\AppData\Roaming").join("magine"));
        let e = Env::from_pairs(&[("XDG_CONFIG_HOME", "/home/a/.cfg")]);
        assert_eq!(config_dir_for(&e, Platform::Other, Path::new("/home/a")), Path::new("/home/a/.cfg").join("magine"));
        let e = Env::from_pairs(&[]);
        assert_eq!(config_dir_for(&e, Platform::Mac, Path::new("/Users/a")), Path::new("/Users/a").join(".config").join("magine"));
    }

    #[test]
    fn secret_files_are_written_atomically_and_private() {
        let dir = std::env::temp_dir().join(format!("magine-test-{}-{}", std::process::id(), crate::util::now_ms()));
        let nested = dir.join("nested");
        let p = write_secret_file(&nested, "credentials.json", &serde_json::json!({ "apiKey": "mk_x" })).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&fs::read_to_string(&p).unwrap()).unwrap()["apiKey"], "mk_x");
        write_secret_file(&nested, "credentials.json", &serde_json::json!({ "apiKey": "mk_y" })).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&fs::read_to_string(&p).unwrap()).unwrap()["apiKey"], "mk_y");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(fs::metadata(&nested).unwrap().permissions().mode() & 0o777, 0o700);
        }
        assert_eq!(fs::read_dir(&nested).unwrap().count(), 1, "no temp files left behind");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn api_key_format() {
        assert!(is_api_key(&format!("mk_{}", "a1".repeat(32))));
        assert!(!is_api_key(&format!("mk_{}", "A1".repeat(32))));
        assert!(!is_api_key("mk_short"));
        assert!(!is_api_key(&format!(" mk_{}", "a1".repeat(32))));
    }
}
