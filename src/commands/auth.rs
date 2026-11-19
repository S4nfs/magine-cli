use crate::api::Call;
use crate::brand;
use crate::commands::guide::VERSION;
use crate::config::{
    credentials_path, delete_credentials, delete_pending_login, is_api_key, load_credentials, load_pending_login, save_credentials, save_pending_login,
    Credentials, PendingLogin,
};
use crate::errors::{exit, usage_error, MagineError, Result};
use crate::open::{can_open_browser, open_browser};
use crate::output::{pairs, Tone};
use crate::prompt;
use crate::registry::{CommandSpec, Ctx, KeySource};
use crate::term;
use crate::util::{group_thousands, iso_time, now_ms, num, opt_text, parse_duration, parse_iso_ms, relative_time, text};
use serde_json::{json, Value};
use std::time::Duration;

/// Anything that looks like an API key (mk_ and at least six hex digits), wherever it appears.
pub fn looks_like_key(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.match_indices("mk_").any(|(i, _)| lower[i + 3..].bytes().take_while(|b| b.is_ascii_hexdigit()).count() >= 6)
}

/// "mk_...(hidden)" in place of every key, so secrets never reach the terminal.
pub fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.to_ascii_lowercase().find("mk_") {
        let hex = rest[i + 3..].bytes().take_while(|b| b.is_ascii_hexdigit()).count();
        out.push_str(&rest[..i]);
        if hex >= 6 {
            out.push_str("mk_...(hidden)");
            rest = &rest[i + 3 + hex..];
        } else {
            out.push_str(&rest[i..i + 3]);
            rest = &rest[i + 3..];
        }
    }
    out.push_str(rest);
    out
}

pub fn os_label() -> String {
    let name = match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    };
    format!("{name} {}", std::env::consts::ARCH)
}

pub fn hostname(ctx: &Ctx) -> String {
    let from_env = ctx.env.non_empty("COMPUTERNAME").or_else(|| ctx.env.non_empty("HOSTNAME")).map(str::to_string);
    let name = from_env.or_else(os_hostname).unwrap_or_else(|| "this computer".into());
    name.chars().take(64).collect()
}

#[cfg(unix)]
fn os_hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    let r = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if r != 0 {
        return None;
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned()).filter(|s| !s.is_empty())
}

#[cfg(not(unix))]
fn os_hostname() -> Option<String> {
    None
}

fn key_on_command_line() -> MagineError {
    usage_error("Never put an API key on the command line: it is saved in your shell history and visible to other programs.")
        .hint("Type it at the hidden prompt of \"magine login --key\", pipe it in (\"magine login --key < key.txt\"), or set MAGINE_API_KEY.")
}

fn choose_step(ctx: &Ctx) -> Result<i32> {
    let data = json!({
        "step": "choose_method",
        "message": "Ask the user how they want to sign in to Magine, then run the command for that method.",
        "methods": [
            { "id": "browser", "command": "magine login --browser --json", "description": "Approve a one-time code in the browser. Recommended: no key to copy." },
            { "id": "api_key", "command": "magine login --key", "description": "Use an API key from \"apikey create\" on magine.cloud. The user types it at the hidden prompt in their own terminal, or sets MAGINE_API_KEY. Never paste a key into chat." }
        ]
    });
    let o = &ctx.out;
    ctx.out.result(&data, || {
        [
            brand::mark(&o.p, "Sign in to Magine"),
            String::new(),
            "How do you want to sign in?".into(),
            format!("  {}   approve a code in your browser (recommended)", o.p.tone(Tone::Green, "magine login --browser")),
            format!("  {}       use an API key (typed at a hidden prompt, piped in, or MAGINE_API_KEY)", o.p.tone(Tone::Green, "magine login --key")),
        ]
        .join("\n")
    });
    Ok(exit::OK)
}

fn signed_in(ctx: &Ctx, creds: Credentials, previous: Option<Credentials>) -> Result<i32> {
    save_credentials(&creds, &ctx.config_dir)?;
    delete_pending_login(&ctx.config_dir);
    // A new browser sign-in replaces this computer's old one; a key the user brought is theirs to keep.
    if let Some(prev) = previous {
        if prev.method == "browser" && prev.api_url == creds.api_url && !prev.key_id.is_empty() && prev.key_id != creds.key_id {
            let _ =
                ctx.api_at(&creds.api_url, Some(creds.api_key.clone()), 15_000).call("DELETE", &format!("keys/{}", crate::api::seg(&prev.key_id)), Call::new());
        }
    }
    let data = json!({
        "step": "done",
        "user": { "username": creds.username },
        "apiUrl": creds.api_url,
        "method": creds.method,
        "keyPrefix": creds.key_prefix,
        "expiresAt": creds.expires_at,
    });
    let o = &ctx.out;
    o.result(&data, || {
        let until = creds.expires_at.as_deref().map(|e| format!(" until {}", &e[..e.len().min(10)])).unwrap_or_default();
        format!("{} {} as {}{until}. Try {}", brand::mark(&o.p, "Magine"), o.ok("Signed in"), o.bold(&creds.username), o.accent("magine agents list"))
    });
    Ok(exit::OK)
}

/// Polls until the browser approves, denies or the code expires; `max_ms` caps one call so an AI agent's command does not time out.
fn wait_for_approval(ctx: &Ctx, pending: &PendingLogin, max_ms: u64, poll_first: bool) -> Result<i32> {
    let api = ctx.api_at(&pending.api_url, None, 20_000);
    let expires = parse_iso_ms(&pending.expires_at).unwrap_or(0);
    let started = now_ms();
    let deadline = (started + max_ms as i64).min(expires);
    let mut interval = pending.interval.max(1);
    let mut first = poll_first;
    let _waiting = term::Waiting::start();
    while now_ms() < deadline {
        if !first {
            term::sleep_ms((interval * 1000).min((deadline - now_ms()).max(0) as u64))?;
        }
        first = false;
        if now_ms() >= deadline {
            break;
        }
        ctx.out.progress(&format!("Waiting for approval in the browser... {}s", (now_ms() - started) / 1000));
        match api.call("POST", "auth/device/token", Call::new().no_auth().json(json!({ "deviceCode": pending.device_code }))) {
            Ok(r) => {
                ctx.out.progress_clear();
                let d = r.data;
                let previous = load_credentials(&ctx.config_dir).ok().flatten();
                return signed_in(
                    ctx,
                    Credentials {
                        version: 1,
                        api_url: pending.api_url.clone(),
                        api_key: text(&d["apiKey"]),
                        key_id: text(&d["keyId"]),
                        key_prefix: text(&d["keyPrefix"]),
                        username: text(&d["user"]["username"]),
                        method: "browser".into(),
                        expires_at: opt_text(&d["expiresAt"]),
                        saved_at: iso_time(now_ms()),
                    },
                    previous,
                );
            }
            Err(e) => match e.code.as_str() {
                "authorization_pending" => continue,
                "slow_down" => {
                    interval = (interval + 5).max(num(e.details.get("interval").unwrap_or(&Value::Null)) as u64);
                    continue;
                }
                "access_denied" | "expired_token" => {
                    ctx.out.progress_clear();
                    delete_pending_login(&ctx.config_dir);
                    let msg = if e.code == "access_denied" {
                        "The sign-in was denied in the browser."
                    } else {
                        "The sign-in code expired. Run \"magine login\" again."
                    };
                    return Err(MagineError::new(msg, exit::AUTH, e.code.as_str()));
                }
                _ if e.exit_code == exit::NETWORK && e.code == "network" => continue,
                _ => {
                    ctx.out.progress_clear();
                    return Err(e);
                }
            },
        }
    }
    ctx.out.progress_clear();
    if expires <= now_ms() {
        delete_pending_login(&ctx.config_dir);
        return Err(MagineError::new("The sign-in code expired. Run \"magine login\" again.", exit::AUTH, "expired_token"));
    }
    let data = json!({
        "step": "waiting",
        "userCode": pending.user_code,
        "verificationUriComplete": pending.verification_uri_complete,
        "expiresAt": pending.expires_at,
        "next": "magine login --wait --json",
    });
    let o = &ctx.out;
    o.result(&data, || {
        format!(
            "Still waiting for approval of {} at {}. Run {} again.",
            o.bold(&pending.user_code),
            o.link(&pending.verification_uri_complete),
            o.accent("\"magine login --wait\"")
        )
    });
    Ok(exit::WAITING)
}

fn browser_login(ctx: &Ctx) -> Result<i32> {
    let body = json!({ "client": format!("Magine CLI {VERSION}"), "hostname": hostname(ctx), "os": os_label() });
    let d = ctx.public_api().call("POST", "auth/device", Call::new().no_auth().json(body))?.data;
    let expires_in = num(&d["expiresIn"]);
    let pending = PendingLogin {
        api_url: ctx.api_url.clone(),
        device_code: text(&d["deviceCode"]),
        user_code: text(&d["userCode"]),
        verification_uri: text(&d["verificationUri"]),
        verification_uri_complete: text(&d["verificationUriComplete"]),
        expires_at: iso_time(now_ms() + (if expires_in > 0.0 { expires_in } else { 600.0 } * 1000.0) as i64),
        interval: num(&d["interval"]).max(0.0) as u64,
    };
    let pending = PendingLogin { interval: if pending.interval == 0 { 5 } else { pending.interval }, ..pending };
    let opened = ctx.opts.not_off("open") && can_open_browser(&ctx.env, std::env::consts::OS) && open_browser(&pending.verification_uri_complete);
    let o = &ctx.out;
    if !ctx.interactive {
        // An AI agent shows the link to its user, then finishes with `magine login --wait`.
        save_pending_login(&pending, &ctx.config_dir)?;
        let data = json!({
            "step": "approve",
            "userCode": pending.user_code,
            "verificationUri": pending.verification_uri,
            "verificationUriComplete": pending.verification_uri_complete,
            "expiresAt": pending.expires_at,
            "browserOpened": opened,
            "message": format!("Ask the user to open {}, check the code {} and approve. Then run \"magine login --wait --json\".", pending.verification_uri_complete, pending.user_code),
            "next": "magine login --wait --json",
        });
        o.result(&data, || {
            [
                brand::mark(&o.p, "Sign in to Magine"),
                format!("  Open     {}", o.link(&pending.verification_uri_complete)),
                format!("  Code     {}", o.p.strong(Tone::Accent, &pending.user_code)),
                format!("  Then run {}", o.accent("magine login --wait")),
            ]
            .join("\n")
        });
        return Ok(exit::OK);
    }
    o.line(&brand::mark(&o.p, "Sign in to Magine"));
    o.line(&format!("  Your one-time code: {}", o.p.strong(Tone::Accent, &pending.user_code)));
    o.line(&format!(
        "  {} {}{} and approve it there.",
        if opened { "Opened" } else { "Open" },
        o.link(&pending.verification_uri_complete),
        if opened { " in your browser" } else { "" }
    ));
    wait_for_approval(ctx, &pending, 15 * 60_000, false)
}

fn read_key(ctx: &Ctx) -> Result<String> {
    if term::stdin_is_tty() {
        if ctx.out.json {
            return Err(usage_error("Type the key in an interactive terminal (without --json), pipe it in, or set MAGINE_API_KEY."));
        }
        ctx.out.note(&format!("{} Create a key on magine.cloud with \"apikey create\", then paste it here (input is hidden).", brand::mark(&ctx.out.e, "")));
        return Ok(prompt::ask_hidden("API key: ")?.trim().to_string());
    }
    let piped = prompt::read_stdin(64 * 1024, Duration::from_secs(2))?;
    if !piped.is_empty() {
        return Ok(piped);
    }
    if let Some(k) = ctx.env.non_empty("MAGINE_API_KEY") {
        return Ok(k.to_string());
    }
    Err(usage_error("No API key given.")
        .hint("Run \"magine login --key\" in a terminal to type it hidden, pipe it in (\"magine login --key < key.txt\"), or set MAGINE_API_KEY."))
}

fn key_login(ctx: &Ctx) -> Result<i32> {
    let key = read_key(ctx)?;
    if !is_api_key(&key) {
        return Err(MagineError::new("That is not a Magine API key (they look like mk_ followed by 64 letters and digits).", exit::AUTH, "unauthorized"));
    }
    let d = ctx.api_at(&ctx.api_url, Some(key.clone()), 60_000).get("me")?.data;
    let previous = load_credentials(&ctx.config_dir).ok().flatten();
    signed_in(
        ctx,
        Credentials {
            version: 1,
            api_url: ctx.api_url.clone(),
            api_key: key.clone(),
            key_id: text(&d["key"]["id"]),
            key_prefix: format!("{}...", &key[..11]),
            username: text(&d["user"]["username"]),
            method: if d["key"]["source"] == "cli" { "browser".into() } else { "key".into() },
            expires_at: opt_text(&d["key"]["expiresAt"]),
            saved_at: iso_time(now_ms()),
        },
        previous,
    )
}

fn login(ctx: &Ctx) -> Result<i32> {
    let arg = ctx.args.first().map(String::as_str);
    if let Some(a) = arg {
        if looks_like_key(a) {
            return Err(key_on_command_line());
        }
        if a != "browser" && a != "key" {
            return Err(usage_error("Sign in with \"magine login --browser\" or \"magine login --key\"."));
        }
    }
    if ctx.opts.on("wait") {
        let Some(pending) = load_pending_login(&ctx.config_dir)? else {
            return Err(usage_error("No browser sign-in is waiting. Start one with \"magine login --browser\"."));
        };
        let max = match ctx.opt("timeout") {
            Some(t) => parse_duration(t, "--timeout")?,
            None => 100_000,
        };
        return wait_for_approval(ctx, &pending, max, true);
    }
    let mut method = if ctx.opts.on("browser") || arg == Some("browser") {
        Some("browser")
    } else if ctx.opts.on("key") || arg == Some("key") {
        Some("key")
    } else {
        None
    };
    if method.is_none() {
        if !ctx.interactive {
            return choose_step(ctx);
        }
        let o = &ctx.out;
        o.line(&brand::mark(&o.p, "Sign in to Magine"));
        let answer = prompt::ask(&format!(
            "  Sign in with {} your browser (recommended) or {} an API key? ",
            o.e.strong(Tone::Accent, "[1]"),
            o.e.strong(Tone::Accent, "[2]")
        ))?;
        let a = answer.trim().to_ascii_lowercase();
        method = Some(if a == "2" || a.starts_with('k') { "key" } else { "browser" });
    }
    if method == Some("browser") {
        browser_login(ctx)
    } else {
        key_login(ctx)
    }
}

fn logout(ctx: &Ctx) -> Result<i32> {
    let creds = load_credentials(&ctx.config_dir)?;
    delete_pending_login(&ctx.config_dir);
    if ctx.key_source == Some(KeySource::Env) {
        ctx.out.warn("MAGINE_API_KEY is set in your environment, so commands still run as that key. Unset it to sign out fully.");
    }
    let Some(creds) = creds else {
        ctx.out.result(&json!({ "signedOut": false, "message": "Not signed in." }), || "Not signed in.".into());
        return Ok(exit::OK);
    };
    let mut revoked = false;
    if creds.method == "browser" || ctx.opts.on("revoke") {
        revoked = match ctx.api_at(&creds.api_url, Some(creds.api_key.clone()), 15_000).call("DELETE", "keys/current", Call::new()) {
            Ok(_) => true,
            Err(e) => e.exit_code == exit::AUTH,
        };
    }
    delete_credentials(&ctx.config_dir);
    let o = &ctx.out;
    o.result(&json!({ "signedOut": true, "username": creds.username, "keyRevoked": revoked }), || {
        if revoked || creds.method != "browser" {
            let who = if creds.username.is_empty() { String::new() } else { format!(" of {}", o.bold(&creds.username)) };
            format!("{}{who}.", o.ok("Signed out"))
        } else {
            format!("{} here, but the key could not be revoked (offline?). Revoke it on magine.cloud with \"apikey revoke\".", o.yellow("Signed out"))
        }
    });
    Ok(exit::OK)
}

fn whoami(ctx: &Ctx) -> Result<i32> {
    let api = ctx.api()?;
    let d = api.get("me")?.data;
    let where_ = if ctx.key_source == Some(KeySource::Env) { "MAGINE_API_KEY".to_string() } else { credentials_path(&ctx.config_dir).display().to_string() };
    let mut data = d.clone();
    if let Some(m) = data.as_object_mut() {
        m.insert("apiUrl".into(), json!(ctx.api_url));
        m.insert("credentials".into(), json!(where_));
        m.insert("cli".into(), json!({ "version": VERSION }));
    }
    let o = &ctx.out;
    o.result(&data, || {
        let email = opt_text(&d["user"]["email"]).map(|e| format!(" ({e})")).unwrap_or_default();
        let expires = opt_text(&d["key"]["expiresAt"]).map(|e| format!(", expires {}", relative_time(Some(&e), now_ms()))).unwrap_or_default();
        let server_version = opt_text(&d["server"]["version"]).or_else(|| api.server_version.borrow().clone()).unwrap_or_else(|| "unknown".into());
        [
            brand::mark(&o.p, "Magine"),
            pairs(
                o,
                vec![
                    ("Signed in as", Some(format!("{}{email}", o.bold(&text(&d["user"]["username"]))))),
                    ("Key", Some(format!("{} [{}]{expires}", opt_text(&d["key"]["name"]).unwrap_or_else(|| "-".into()), text(&d["key"]["source"])))),
                    (
                        "Wallet",
                        Some(format!(
                            "{} tokens, {} Cats, {} agent slots",
                            o.bold(&group_thousands(num(&d["wallet"]["tokens"]))),
                            text(&d["wallet"]["cats"]),
                            text(&d["wallet"]["agentSlots"])
                        )),
                    ),
                    ("Server", Some(format!("{} ({server_version})", o.link(&ctx.api_url)))),
                    ("CLI", Some(VERSION.to_string())),
                    ("Credentials", Some(where_.clone())),
                ],
            ),
        ]
        .join("\n")
    });
    Ok(exit::OK)
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("login", "Account", "Sign in with your browser or an API key", login)
            .desc(
                "Sign in to Magine. Two ways:\n\
                 \x20 --browser  You approve a one-time code on magine.cloud/device. The CLI then gets a key of its own\n\
                 \x20            for this computer (90 days). Nothing to copy, and the password never leaves the browser.\n\
                 \x20 --key      Use an API key you made on magine.cloud (\"apikey create\"). It is read at a hidden prompt\n\
                 \x20            or from stdin, never from the command line. MAGINE_API_KEY works without signing in.\n\
                 Without --json in a terminal, \"magine login\" asks which one. AI agents: run \"magine login --json\"\n\
                 and ask your user which method to use.",
            )
            .arg("method", "\"browser\" or \"key\" (same as --browser / --key)", false, false)
            .flag("browser", "Approve a one-time code in your browser (recommended)")
            .flag("key", "Use an API key typed at a hidden prompt, piped in, or from MAGINE_API_KEY")
            .flag("wait", "Finish a browser sign-in started with --json")
            .value("timeout", "duration", "How long --wait waits before answering \"waiting\" (default 100s)")
            .flag("open", "Open the browser (--no-open only prints the link)")
            .examples(&["magine login", "magine login --browser", "magine login --key < key.txt", "magine login --browser --json   # then: magine login --wait --json"]),
        CommandSpec::new("logout", "Account", "Sign out on this computer", logout)
            .desc("Removes the saved sign-in. A key from a browser sign-in is revoked on Magine too; a key you brought with --key is kept unless you add --revoke.")
            .flag("revoke", "Also revoke an API key you signed in with --key"),
        CommandSpec::new("whoami", "Account", "Show who you are signed in as, your key and wallet", whoami).aliases(&["status"]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_never_reach_the_terminal() {
        let key = format!("mk_{}", "ab".repeat(32));
        assert_eq!(redact(&format!("bad {key} here")), "bad mk_...(hidden) here");
        assert_eq!(redact("mk_ and mk_12 stay"), "mk_ and mk_12 stay");
        assert!(looks_like_key(&key));
        assert!(looks_like_key("MK_ABCDEF12"));
        assert!(!looks_like_key("browser"));
    }
}
