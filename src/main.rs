//! The Magine CLI: your Magine agents from any terminal or AI coding agent.

mod api;
mod brand;
mod commands;
mod config;
mod editor;
mod env;
mod errors;
mod http;
mod open;
mod output;
mod prompt;
mod registry;
mod tags;
mod term;
mod util;

use commands::all_commands;
use commands::auth::redact;
use commands::guide::VERSION;
use config::{config_dir, is_api_key, load_credentials, normalize_api_url, DEFAULT_API_URL};
use env::Env;
use errors::{exit, usage_error, MagineError, Result};
use output::{Output, Painter};
use registry::{check_args, command_help, group_help, main_help, not_signed_in, parse_argv, CommandSpec, Ctx, KeySource};
use serde_json::json;
use std::cell::OnceCell;

/// Commands that work without a sign-in.
const OPEN_COMMANDS: [&str; 4] = ["login", "logout", "guide", "help"];

fn main() {
    let argv: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let code = run(&argv, &Env::from_process());
    std::process::exit(code);
}

fn run(argv: &[String], env: &Env) -> i32 {
    term::install_interrupt_handler();
    let json = argv.iter().any(|a| matches!(a.as_str(), "--json" | "--json=true" | "--json=1" | "--json=yes"));
    term::set_json_errors(json);
    let ansi = term::enable_ansi();
    let no_color = argv.iter().any(|a| matches!(a.as_str(), "--no-color" | "--color=false" | "--color=0" | "--color=no"));
    let truecolor = output::wants_truecolor(env);
    let out_color = ansi && !no_color && output::wants_color(term::stdout_is_tty(), env);
    let err_color = ansi && !no_color && output::wants_color(term::stderr_is_tty(), env);
    let make = || Output::new(json, out_color, err_color, truecolor);
    let errors = make();
    match execute(argv, env, make(), json) {
        Ok(code) => code,
        Err(mut e) => {
            e.message = redact(&e.message);
            e.hint = e.hint.as_deref().map(redact);
            errors.error(&e);
            e.exit_code
        }
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        prev = cur;
    }
    prev[b.len()]
}

fn suggestion(words: &[String], commands: &[CommandSpec]) -> Option<String> {
    let typed = words.join(" ").to_lowercase();
    let best = commands
        .iter()
        .flat_map(|c| std::iter::once(c.name).chain(c.aliases.iter().copied()))
        .map(|n| (n, edit_distance(&typed, n)))
        .min_by_key(|(_, d)| *d)?;
    (best.1 <= 2.max(typed.chars().count() / 3)).then(|| format!("Did you mean \"magine {}\"?", best.0))
}

fn execute(argv: &[String], env: &Env, out: Output, json: bool) -> Result<i32> {
    let cmds = all_commands();
    let parsed = parse_argv(argv, cmds)?;
    let plain = Painter::default();
    if parsed.opts.on("version") {
        out.result(&json!({ "version": VERSION }), || VERSION.to_string());
        return Ok(exit::OK);
    }
    let Some(command) = parsed.command else {
        if parsed.lead.is_empty() {
            out.result(&json!({ "help": main_help(cmds, VERSION, &plain, false) }), || main_help(cmds, VERSION, &out.p, true));
            return Ok(exit::OK);
        }
        let first = parsed.lead[0].to_lowercase();
        if parsed.opts.on("help") {
            if let Some(text) = group_help(cmds, &first, &plain) {
                out.result(&json!({ "help": text }), || group_help(cmds, &first, &out.p).unwrap_or_default());
                return Ok(exit::OK);
            }
        }
        return Err(usage_error(format!("Unknown command \"magine {}\".", parsed.lead.join(" ")))
            .hint(suggestion(&parsed.lead, cmds).unwrap_or_else(|| "See \"magine --help\".".into())));
    };
    // `magine agents` lists; `magine agents --help` lists the agents commands.
    if parsed.opts.on("help") {
        let group = if parsed.matched.len() == 1 && command.name.contains(' ') { Some(parsed.matched[0].to_lowercase()) } else { None };
        let render = |p: &Painter| group.as_deref().and_then(|g| group_help(cmds, g, p)).unwrap_or_else(|| command_help(command, p));
        out.result(&json!({ "help": render(&plain) }), || render(&out.p));
        return Ok(exit::OK);
    }
    if parsed.matched.len() == 1 && command.name.contains(' ') && command.args.is_empty() && !parsed.args.is_empty() {
        let words = vec![parsed.matched[0].clone(), parsed.args[0].clone()];
        let hint = suggestion(&words, cmds).unwrap_or_else(|| format!("See \"magine {} --help\".", parsed.matched[0]));
        return Err(usage_error(format!("Unknown command \"magine {}\".", words.join(" "))).hint(hint));
    }
    check_args(command, &parsed.args)?;

    let dir = config_dir(env);
    let creds = load_credentials(&dir)?;
    let raw_url = parsed
        .opts
        .str("api-url")
        .map(str::to_string)
        .or_else(|| env.non_empty("MAGINE_API_URL").map(str::to_string))
        .or_else(|| creds.as_ref().map(|c| c.api_url.clone()));
    let api_url = normalize_api_url(raw_url.as_deref().unwrap_or(DEFAULT_API_URL))?;
    let env_key = env.non_empty("MAGINE_API_KEY").map(str::to_string);
    if env_key.as_deref().is_some_and(|k| !is_api_key(k)) {
        return Err(MagineError::new("MAGINE_API_KEY is set but is not a Magine API key.", exit::AUTH, "unauthorized")
            .hint("Keys look like mk_ followed by 64 letters and digits."));
    }
    let saved = creds.as_ref().filter(|c| c.api_url == api_url).cloned();
    let key_source = if env_key.is_some() {
        Some(KeySource::Env)
    } else if saved.is_some() {
        Some(KeySource::File)
    } else {
        None
    };
    let key = env_key.or_else(|| saved.map(|c| c.api_key));
    let ctx = Ctx {
        args: parsed.args,
        opts: parsed.opts,
        out,
        api_url,
        config_dir: dir,
        env: env.clone(),
        interactive: !json && term::stdin_is_tty() && term::stdout_is_tty(),
        key_source,
        key,
        creds,
        http: OnceCell::new(),
    };
    if !OPEN_COMMANDS.contains(&command.name) && ctx.key.is_none() {
        return Err(not_signed_in(ctx.creds.as_ref(), &ctx.api_url));
    }
    (command.run)(&ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typos_get_a_suggestion() {
        let s = suggestion(&["agnets".into(), "list".into()], all_commands());
        assert_eq!(s.as_deref(), Some("Did you mean \"magine agents list\"?"));
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert!(suggestion(&["completely-unrelated-words".into()], all_commands()).is_none());
    }
}
