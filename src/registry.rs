use crate::api::Api;
use crate::brand;
use crate::config::Credentials;
use crate::env::Env;
use crate::errors::{exit, usage_error, MagineError, Result};
use crate::http::Http;
use crate::output::{Output, Painter, Tone};
use std::cell::OnceCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

pub struct OptionSpec {
    pub name: &'static str,
    pub alias: Option<&'static str>,
    pub takes_value: bool,
    pub placeholder: &'static str,
    pub description: &'static str,
}

pub struct ArgSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub required: bool,
    pub variadic: bool,
}

pub type RunFn = fn(&Ctx) -> Result<i32>;

pub struct CommandSpec {
    pub name: &'static str,
    pub group: &'static str,
    pub summary: &'static str,
    pub description: Option<&'static str>,
    pub args: Vec<ArgSpec>,
    pub options: Vec<OptionSpec>,
    pub examples: Vec<&'static str>,
    pub aliases: Vec<&'static str>,
    /// Spends tokens or Cats (listed in `magine guide`).
    pub costs: Option<&'static str>,
    pub run: RunFn,
}

impl CommandSpec {
    pub fn new(name: &'static str, group: &'static str, summary: &'static str, run: RunFn) -> Self {
        CommandSpec { name, group, summary, description: None, args: vec![], options: vec![], examples: vec![], aliases: vec![], costs: None, run }
    }
    pub fn aliases(mut self, a: &[&'static str]) -> Self {
        self.aliases = a.to_vec();
        self
    }
    pub fn desc(mut self, d: &'static str) -> Self {
        self.description = Some(d);
        self
    }
    pub fn costs(mut self, c: &'static str) -> Self {
        self.costs = Some(c);
        self
    }
    pub fn examples(mut self, e: &[&'static str]) -> Self {
        self.examples = e.to_vec();
        self
    }
    pub fn arg(mut self, name: &'static str, description: &'static str, required: bool, variadic: bool) -> Self {
        self.args.push(ArgSpec { name, description, required, variadic });
        self
    }
    pub fn flag(mut self, name: &'static str, description: &'static str) -> Self {
        self.options.push(OptionSpec { name, alias: None, takes_value: false, placeholder: "", description });
        self
    }
    pub fn flag_alias(mut self, name: &'static str, alias: &'static str, description: &'static str) -> Self {
        self.options.push(OptionSpec { name, alias: Some(alias), takes_value: false, placeholder: "", description });
        self
    }
    pub fn value(mut self, name: &'static str, placeholder: &'static str, description: &'static str) -> Self {
        self.options.push(OptionSpec { name, alias: None, takes_value: true, placeholder, description });
        self
    }
    pub fn value_alias(mut self, name: &'static str, alias: &'static str, placeholder: &'static str, description: &'static str) -> Self {
        self.options.push(OptionSpec { name, alias: Some(alias), takes_value: true, placeholder, description });
        self
    }
}

pub fn global_options() -> Vec<OptionSpec> {
    vec![
        OptionSpec { name: "json", alias: None, takes_value: false, placeholder: "", description: "Print the result as JSON (for scripts and AI agents)" },
        OptionSpec {
            name: "api-url",
            alias: None,
            takes_value: true,
            placeholder: "url",
            description: "Magine server (default https://magine.cloud, or MAGINE_API_URL)",
        },
        OptionSpec { name: "color", alias: None, takes_value: false, placeholder: "", description: "Colours (--no-color turns them off; so does NO_COLOR)" },
        OptionSpec { name: "help", alias: Some("h"), takes_value: false, placeholder: "", description: "Show help for a command" },
        OptionSpec { name: "version", alias: Some("v"), takes_value: false, placeholder: "", description: "Show the CLI version" },
    ]
}

#[derive(Clone, Debug, PartialEq)]
pub enum OptValue {
    Bool(bool),
    Str(String),
}

#[derive(Default, Debug)]
pub struct Opts(HashMap<String, OptValue>);

impl Opts {
    pub fn str(&self, name: &str) -> Option<&str> {
        match self.0.get(name) {
            Some(OptValue::Str(s)) => Some(s),
            _ => None,
        }
    }
    pub fn bool(&self, name: &str) -> Option<bool> {
        match self.0.get(name) {
            Some(OptValue::Bool(b)) => Some(*b),
            _ => None,
        }
    }
    /// The switch was given (and not as --no-...).
    pub fn on(&self, name: &str) -> bool {
        self.bool(name) == Some(true)
    }
    /// Not turned off with --no-...
    pub fn not_off(&self, name: &str) -> bool {
        self.bool(name) != Some(false)
    }
    pub fn set(&mut self, name: &str, v: OptValue) {
        self.0.insert(name.to_string(), v);
    }
}

pub struct Parsed<'a> {
    pub command: Option<&'a CommandSpec>,
    /// The words that named the command, as typed.
    pub matched: Vec<String>,
    /// The first words given, whether or not they named a command.
    pub lead: Vec<String>,
    pub args: Vec<String>,
    pub opts: Opts,
}

/// The longest command name matching the leading words of `tokens`.
pub fn find_command<'a>(tokens: &[String], commands: &'a [CommandSpec]) -> (Option<&'a CommandSpec>, usize) {
    let mut best: (Option<&CommandSpec>, usize) = (None, 0);
    for c in commands {
        for name in std::iter::once(&c.name).chain(c.aliases.iter()) {
            let words: Vec<&str> = name.split(' ').collect();
            if words.len() > tokens.len() || words.len() <= best.1 {
                continue;
            }
            if words.iter().enumerate().all(|(i, w)| *w == tokens[i].to_lowercase()) {
                best = (Some(c), words.len());
            }
        }
    }
    best
}

pub fn parse_argv<'a>(argv: &[String], commands: &'a [CommandSpec]) -> Result<Parsed<'a>> {
    let globals = global_options();
    // The first words (skipping options) name the command; global options may come first.
    let mut lead = Vec::new();
    let mut lead_at = Vec::new();
    let mut i = 0;
    while i < argv.len() && lead.len() < 3 {
        let t = &argv[i];
        if t == "--" {
            break;
        }
        if t.starts_with('-') && t != "-" {
            if globals.iter().any(|g| g.takes_value && *t == format!("--{}", g.name)) {
                i += 1;
            }
            i += 1;
            continue;
        }
        lead.push(t.clone());
        lead_at.push(i);
        i += 1;
    }
    let (command, used) = find_command(&lead, commands);
    let skip: Vec<usize> = lead_at[..used].to_vec();
    let rest: Vec<&String> = argv.iter().enumerate().filter(|(i, _)| !skip.contains(i)).map(|(_, t)| t).collect();
    let specs: Vec<&OptionSpec> = globals.iter().chain(command.map(|c| c.options.iter()).into_iter().flatten()).collect();
    let where_ = command.map(|c| format!("\"magine {}\"", c.name)).unwrap_or_else(|| "magine".into());
    let see = || format!("See \"magine {}--help\".", command.map(|c| format!("{} ", c.name)).unwrap_or_default());
    let mut opts = Opts::default();
    let mut args = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i].as_str();
        if t == "--" {
            args.extend(rest[i + 1..].iter().map(|s| s.to_string()));
            break;
        }
        if t == "-" || !t.starts_with('-') {
            args.push(t.to_string());
            i += 1;
            continue;
        }
        let (spec, inline) = if let Some(long) = t.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) if !n.is_empty() => (n, Some(v.to_string())),
                _ => (long, None),
            };
            let spec = specs.iter().find(|s| s.name == name).copied();
            if spec.is_none() {
                if let Some(base) = name.strip_prefix("no-") {
                    if specs.iter().any(|s| s.name == base && !s.takes_value) {
                        if inline.is_some() {
                            return Err(usage_error(format!("--{name} does not take a value.")));
                        }
                        opts.set(base, OptValue::Bool(false));
                        i += 1;
                        continue;
                    }
                }
            }
            (spec, inline)
        } else {
            (specs.iter().find(|s| s.alias == Some(&t[1..])).copied(), None)
        };
        let Some(spec) = spec else {
            return Err(usage_error(format!("Unknown option {} for {where_}.", t.split('=').next().unwrap_or(t))).hint(see()));
        };
        if !spec.takes_value {
            match inline {
                Some(v) => {
                    let v = v.to_ascii_lowercase();
                    if !["true", "false", "1", "0", "yes", "no"].contains(&v.as_str()) {
                        return Err(usage_error(format!("--{} is a yes/no switch.", spec.name)));
                    }
                    opts.set(spec.name, OptValue::Bool(matches!(v.as_str(), "true" | "1" | "yes")));
                }
                None => opts.set(spec.name, OptValue::Bool(true)),
            }
            i += 1;
            continue;
        }
        let value = match inline {
            Some(v) => Some(v),
            None => {
                i += 1;
                rest.get(i).map(|s| s.to_string())
            }
        };
        let Some(value) = value else {
            return Err(usage_error(format!("--{} needs a value.", spec.name)).hint(see()));
        };
        opts.set(spec.name, OptValue::Str(value));
        i += 1;
    }
    Ok(Parsed { command, matched: lead[..used].to_vec(), lead, args, opts })
}

/// Checks the positional arguments against the command's spec.
pub fn check_args(command: &CommandSpec, args: &[String]) -> Result<()> {
    let required: Vec<&ArgSpec> = command.args.iter().filter(|a| a.required).collect();
    let variadic = command.args.iter().any(|a| a.variadic);
    if args.len() < required.len() {
        let missing = required[args.len()];
        return Err(usage_error(format!("Missing <{}>: {}", missing.name, missing.description)).hint(format!("Usage: {}", usage_line(command))));
    }
    if !variadic && args.len() > command.args.len() {
        return Err(usage_error(format!("Unexpected argument \"{}\".", args[command.args.len()])).hint(format!("Usage: {}", usage_line(command))));
    }
    Ok(())
}

pub fn usage_line(c: &CommandSpec) -> String {
    let mut parts = vec!["magine".to_string(), c.name.to_string()];
    for a in &c.args {
        let dots = if a.variadic { "..." } else { "" };
        parts.push(if a.required { format!("<{}{dots}>", a.name) } else { format!("[{}{dots}]", a.name) });
    }
    if !c.options.is_empty() {
        parts.push("[options]".into());
    }
    parts.join(" ")
}

fn option_line(p: &Painter, o: &OptionSpec) -> String {
    let flag = format!(
        "{}--{}{}",
        o.alias.map(|a| format!("-{a}, ")).unwrap_or_else(|| "    ".into()),
        o.name,
        if o.takes_value { format!(" <{}>", if o.placeholder.is_empty() { "value" } else { o.placeholder }) } else { String::new() }
    );
    if flag.len() > 30 {
        format!("  {}\n  {:30} {}", p.tone(Tone::Green, &flag), "", o.description)
    } else {
        format!("  {} {}", p.tone(Tone::Green, &format!("{flag:<30}")), o.description)
    }
}

pub fn command_help(c: &CommandSpec, p: &Painter) -> String {
    let mut lines = vec![format!("{} {}", p.bold("Usage:"), p.accent(&usage_line(c))), String::new(), c.description.unwrap_or(c.summary).to_string()];
    if let Some(cost) = c.costs {
        lines.push(String::new());
        lines.push(format!("{} {}", p.strong(Tone::Yellow, "Cost:"), cost));
    }
    if !c.args.is_empty() {
        lines.push(String::new());
        lines.push(brand::section(p, "Arguments"));
        for a in &c.args {
            lines.push(format!("  {} {}", p.tone(Tone::Green, &format!("{:<30}", a.name)), a.description));
        }
    }
    if !c.options.is_empty() {
        lines.push(String::new());
        lines.push(brand::section(p, "Options"));
        lines.extend(c.options.iter().map(|o| option_line(p, o)));
    }
    lines.push(String::new());
    lines.push(brand::section(p, "Global options"));
    lines.extend(global_options().iter().map(|o| option_line(p, o)));
    if !c.examples.is_empty() {
        lines.push(String::new());
        lines.push(brand::section(p, "Examples"));
        lines.extend(c.examples.iter().map(|e| format!("  {}", p.muted(e))));
    }
    lines.join("\n")
}

pub fn main_help(commands: &[CommandSpec], version: &str, p: &Painter, with_banner: bool) -> String {
    let mut groups: Vec<(&str, Vec<&CommandSpec>)> = Vec::new();
    for c in commands {
        match groups.iter_mut().find(|(g, _)| *g == c.group) {
            Some((_, list)) => list.push(c),
            None => groups.push((c.group, vec![c])),
        }
    }
    let width = commands.iter().map(|c| c.name.len()).max().unwrap_or(10) + 2;
    let mut lines = Vec::new();
    if with_banner {
        lines.push(brand::banner(p, version, "your AI agents from any terminal or AI coding agent"));
    } else {
        lines.push(format!("Magine CLI {version}: your Magine agents from any terminal or AI coding agent."));
    }
    lines.push(String::new());
    lines.push(format!("{} {}", p.bold("Usage:"), p.accent("magine <command> [options]")));
    for (group, list) in groups {
        lines.push(String::new());
        lines.push(brand::section(p, group));
        for c in list {
            lines.push(format!("  {} {}", p.tone(Tone::Green, &format!("{:<width$}", c.name)), c.summary));
        }
    }
    lines.push(String::new());
    lines.push(brand::section(p, "Global options"));
    lines.extend(global_options().iter().map(|o| option_line(p, o)));
    lines.push(String::new());
    lines.push(format!(
        "Start with {}. {} explains everything for AI agents. Docs: {}",
        p.accent("\"magine login\""),
        p.accent("\"magine guide\""),
        p.link("https://magine.cloud/docs#cli")
    ));
    lines.join("\n")
}

pub fn group_help(commands: &[CommandSpec], prefix: &str, p: &Painter) -> Option<String> {
    let list: Vec<&CommandSpec> = commands.iter().filter(|c| c.name.starts_with(&format!("{prefix} "))).collect();
    if list.is_empty() {
        return None;
    }
    let width = list.iter().map(|c| c.name.len()).max().unwrap_or(10) + 2;
    let mut lines = vec![
        format!("{} {}", p.bold("Usage:"), p.accent(&format!("magine {prefix} <command> [options]"))),
        String::new(),
        brand::section(p, &format!("{prefix} commands")),
    ];
    lines.extend(list.iter().map(|c| format!("  {} {}", p.tone(Tone::Green, &format!("{:<width$}", c.name)), c.summary)));
    lines.push(String::new());
    lines.push(format!("Help for one: {}", p.accent(&format!("magine {prefix} <command> --help"))));
    Some(lines.join("\n"))
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum KeySource {
    Env,
    File,
}

pub struct Ctx {
    pub args: Vec<String>,
    pub opts: Opts,
    pub out: Output,
    pub api_url: String,
    pub config_dir: PathBuf,
    pub env: Env,
    pub interactive: bool,
    pub key_source: Option<KeySource>,
    pub key: Option<String>,
    pub creds: Option<Credentials>,
    pub http: OnceCell<Arc<Http>>,
}

pub fn not_signed_in(creds: Option<&Credentials>, api_url: &str) -> MagineError {
    if let Some(c) = creds {
        if c.api_url != api_url {
            return MagineError::new(format!("You are signed in to {}, not {api_url}.", c.api_url), exit::AUTH, "unauthorized")
                .hint(format!("Run \"magine login --api-url {api_url}\", or drop --api-url / MAGINE_API_URL."));
        }
    }
    MagineError::new("You are not signed in.", exit::AUTH, "unauthorized").hint("Run \"magine login\" (or set MAGINE_API_KEY).")
}

impl Ctx {
    pub fn http(&self) -> Arc<Http> {
        self.http.get_or_init(|| Arc::new(Http::new(&self.env, Some(&self.out)))).clone()
    }

    /// The API with the signed-in key; an error when there is none or it has expired.
    pub fn api(&self) -> Result<Api> {
        let Some(key) = &self.key else { return Err(not_signed_in(self.creds.as_ref(), &self.api_url)) };
        if self.key_source == Some(KeySource::File) {
            if let Some(exp) = self.creds.as_ref().and_then(|c| c.expires_at.as_deref()).and_then(crate::util::parse_iso_ms) {
                if exp <= crate::util::now_ms() {
                    return Err(MagineError::new("Your sign-in on this computer has expired.", exit::AUTH, "key_expired").hint("Run \"magine login\" again."));
                }
            }
        }
        Ok(Api::new(&self.api_url, Some(key.clone()), self.http(), 60_000))
    }

    /// The API without a key (sign-in endpoints).
    pub fn public_api(&self) -> Api {
        Api::new(&self.api_url, None, self.http(), 60_000)
    }

    /// The API at `base` with `key`, for sign-in and sign-out.
    pub fn api_at(&self, base: &str, key: Option<String>, timeout_ms: u64) -> Api {
        Api::new(base, key, self.http(), timeout_ms)
    }

    pub fn opt(&self, name: &str) -> Option<&str> {
        self.opts.str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::all_commands;

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn commands_aliases_and_global_options_anywhere() {
        let cmds = all_commands();
        let p = parse_argv(&argv(&["agents", "run", "news-bot", "--task", "only today", "--json"]), cmds).unwrap();
        assert_eq!(p.command.unwrap().name, "agents run");
        assert_eq!(p.args, vec!["news-bot"]);
        assert_eq!(p.opts.str("task"), Some("only today"));
        assert!(p.opts.on("json"));

        let p = parse_argv(&argv(&["--json", "--api-url", "https://x.example", "agents", "list"]), cmds).unwrap();
        assert_eq!(p.command.unwrap().name, "agents list");
        assert_eq!(p.opts.str("api-url"), Some("https://x.example"));

        assert_eq!(parse_argv(&argv(&["agents"]), cmds).unwrap().command.unwrap().name, "agents list");
        assert_eq!(parse_argv(&argv(&["balance"]), cmds).unwrap().command.unwrap().name, "wallet");
        let p = parse_argv(&argv(&["agents", "run", "x", "--no-wait", "--timeout=5m"]), cmds).unwrap();
        assert_eq!(p.opts.bool("wait"), Some(false));
        assert_eq!(p.opts.str("timeout"), Some("5m"));
        let p = parse_argv(&argv(&["do", "--", "--not-an-option", "task"]), cmds).unwrap();
        assert_eq!(p.args, vec!["--not-an-option", "task"]);
        let p = parse_argv(&argv(&["cattery", "download", "abc", "-o", "-"]), cmds).unwrap();
        assert_eq!(p.opts.str("output"), Some("-"));
        let p = parse_argv(&argv(&["agents", "create", "--prompt", "run", "extra"]), cmds).unwrap();
        assert_eq!(p.command.unwrap().name, "agents create");
        assert_eq!(p.opts.str("prompt"), Some("run"));
        assert_eq!(p.args, vec!["extra"]);
    }

    #[test]
    fn mistakes_are_usage_errors() {
        let cmds = all_commands();
        for bad in [
            &["agents", "list", "--bogus"][..],
            &["agents", "run", "x", "--task"],
            &["agents", "run", "x", "--wait=maybe"],
            &["agents", "run", "x", "--no-task"],
        ] {
            assert_eq!(parse_argv(&argv(bad), cmds).err().map(|e| e.exit_code), Some(exit::USAGE), "{bad:?}");
        }
        let run = find_command(&argv(&["agents", "run"]), cmds).0.unwrap();
        assert_eq!(check_args(run, &[]).unwrap_err().exit_code, exit::USAGE);
        assert_eq!(check_args(run, &argv(&["a", "b"])).unwrap_err().exit_code, exit::USAGE);
        assert!(check_args(run, &argv(&["a"])).is_ok());
        assert_eq!(usage_line(run), "magine agents run <agent> [options]");
    }

    #[test]
    fn every_command_has_a_summary_a_group_and_a_unique_name() {
        let mut names = std::collections::HashSet::new();
        for c in all_commands() {
            assert!(!c.summary.is_empty() && !c.group.is_empty(), "{}", c.name);
            for n in std::iter::once(&c.name).chain(c.aliases.iter()) {
                assert!(names.insert(*n), "duplicate {n}");
            }
        }
    }

    #[test]
    fn help_is_plain_ascii_without_colours() {
        let p = Painter::default();
        let cmds = all_commands();
        let main = main_help(cmds, "1.7.5", &p, true);
        assert!(main.is_ascii(), "{main}");
        assert!(main.contains("magine login"));
        let run = find_command(&argv(&["agents", "run"]), cmds).0.unwrap();
        assert!(command_help(run, &p).starts_with("Usage: magine agents run <agent> [options]"));
        assert!(group_help(cmds, "agents", &p).unwrap().contains("magine agents <command>"));
    }
}
