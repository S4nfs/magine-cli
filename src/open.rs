use crate::env::Env;
use std::process::{Command, Stdio};

/// Whether a browser can be opened here: not in CI, over SSH, without a display, or when MAGINE_NO_BROWSER is set.
pub fn can_open_browser(env: &Env, os: &str) -> bool {
    if ["MAGINE_NO_BROWSER", "CI", "SSH_CONNECTION", "SSH_TTY"].iter().any(|v| env.non_empty(v).is_some()) {
        return false;
    }
    match os {
        "windows" | "macos" => true,
        _ => env.non_empty("DISPLAY").is_some() || env.non_empty("WAYLAND_DISPLAY").is_some(),
    }
}

fn safe_url(url: &str) -> bool {
    let ok_chars = !url.chars().any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>'));
    let Ok(u) = url::Url::parse(url) else { return false };
    let local = matches!(u.host_str(), Some("localhost") | Some("127.0.0.1"));
    ok_chars && (u.scheme() == "https" || (u.scheme() == "http" && local))
}

/// Opens a web address in the default browser, without a shell, so the address is never parsed as a command.
pub fn open_browser(url: &str) -> bool {
    if !safe_url(url) {
        return false;
    }
    let (cmd, args): (&str, Vec<&str>) = match std::env::consts::OS {
        "windows" => ("rundll32.exe", vec!["url.dll,FileProtocolHandler", url]),
        "macos" => ("open", vec![url]),
        _ => ("xdg-open", vec![url]),
    };
    Command::new(cmd).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_opening_is_off_in_ci_over_ssh_and_without_a_display() {
        assert!(!can_open_browser(&Env::from_pairs(&[("CI", "true")]), "windows"));
        assert!(!can_open_browser(&Env::from_pairs(&[("SSH_CONNECTION", "1 2 3 4")]), "macos"));
        assert!(!can_open_browser(&Env::from_pairs(&[]), "linux"));
        assert!(can_open_browser(&Env::from_pairs(&[("DISPLAY", ":0")]), "linux"));
        assert!(can_open_browser(&Env::from_pairs(&[]), "windows"));
        assert!(!can_open_browser(&Env::from_pairs(&[("MAGINE_NO_BROWSER", "1")]), "macos"));
    }

    #[test]
    fn only_web_addresses_are_opened() {
        assert!(safe_url("https://magine.cloud/device?code=WDJB-MJHT"));
        assert!(safe_url("http://localhost:3000/device"));
        assert!(!safe_url("http://evil.example/x"));
        assert!(!safe_url("file:///etc/passwd"));
        assert!(!safe_url("https://magine.cloud/x\" calc"));
        assert!(safe_url("https://checkout.example.com/pay?a=1&b=2"));
    }
}
