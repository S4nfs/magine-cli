use crate::errors::MagineError;
use crate::term;
use serde_json::Value;
use std::cell::Cell as StdCell;
use std::io::Write;

/// Magine's colours (the website's GitHub-dark palette), with a 16-colour fallback.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tone {
    Accent,
    Link,
    Cat,
    Green,
    Yellow,
    Red,
    Muted,
}

impl Tone {
    fn sgr(self, truecolor: bool) -> &'static str {
        match (self, truecolor) {
            (Tone::Accent, true) => "38;2;88;166;255",
            (Tone::Link, true) => "38;2;121;192;255",
            (Tone::Cat, true) => "38;2;163;113;247",
            (Tone::Green, true) => "38;2;63;185;80",
            (Tone::Yellow, true) => "38;2;210;153;34",
            (Tone::Red, true) => "38;2;248;81;73",
            (Tone::Muted, true) => "38;2;139;148;158",
            (Tone::Accent, false) => "94",
            (Tone::Link, false) => "96",
            (Tone::Cat, false) => "95",
            (Tone::Green, false) => "32",
            (Tone::Yellow, false) => "33",
            (Tone::Red, false) => "31",
            (Tone::Muted, false) => "90",
        }
    }
}

/// Paints text for one stream; does nothing when that stream gets no colours.
#[derive(Clone, Copy, Debug, Default)]
pub struct Painter {
    pub on: bool,
    pub truecolor: bool,
}

impl Painter {
    pub fn tone(&self, tone: Tone, t: &str) -> String {
        if !self.on || t.is_empty() {
            return t.to_string();
        }
        format!("\x1b[{}m{}\x1b[39m", tone.sgr(self.truecolor), t)
    }

    pub fn strong(&self, tone: Tone, t: &str) -> String {
        if !self.on || t.is_empty() {
            return t.to_string();
        }
        format!("\x1b[1;{}m{}\x1b[22;39m", tone.sgr(self.truecolor), t)
    }

    pub fn bold(&self, t: &str) -> String {
        if !self.on || t.is_empty() {
            return t.to_string();
        }
        format!("\x1b[1m{t}\x1b[22m")
    }

    pub fn accent(&self, t: &str) -> String {
        self.tone(Tone::Accent, t)
    }
    pub fn link(&self, t: &str) -> String {
        self.tone(Tone::Link, t)
    }
    pub fn green(&self, t: &str) -> String {
        self.tone(Tone::Green, t)
    }
    pub fn yellow(&self, t: &str) -> String {
        self.tone(Tone::Yellow, t)
    }
    pub fn red(&self, t: &str) -> String {
        self.tone(Tone::Red, t)
    }
    pub fn muted(&self, t: &str) -> String {
        self.tone(Tone::Muted, t)
    }
}

/// Colours only on a terminal that wants them (NO_COLOR, --no-color and TERM=dumb turn them off).
pub fn wants_color(is_tty: bool, env: &crate::env::Env) -> bool {
    if env.get("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return false;
    }
    if env.get("FORCE_COLOR").is_some_and(|v| !v.is_empty() && v != "0") {
        return true;
    }
    if env.get("TERM") == Some("dumb") {
        return false;
    }
    is_tty
}

/// 24-bit colour where the terminal says so (and on Windows 10+ consoles, which all have it).
pub fn wants_truecolor(env: &crate::env::Env) -> bool {
    if matches!(env.get("COLORTERM"), Some("truecolor") | Some("24bit")) || env.get("WT_SESSION").is_some() {
        return true;
    }
    if matches!(env.get("TERM_PROGRAM"), Some("vscode") | Some("iTerm.app") | Some("WezTerm") | Some("ghostty")) {
        return true;
    }
    cfg!(windows) && env.get("FORCE_COLOR").is_none_or(|v| v != "1" && v != "2")
}

pub struct Output {
    pub json: bool,
    /// Paints what goes to stdout.
    pub p: Painter,
    /// Paints what goes to stderr.
    pub e: Painter,
    stderr_tty: bool,
    progress_len: StdCell<usize>,
}

impl Output {
    pub fn new(json: bool, out_color: bool, err_color: bool, truecolor: bool) -> Self {
        Output {
            json,
            p: Painter { on: out_color && !json, truecolor },
            e: Painter { on: err_color && !json, truecolor },
            stderr_tty: term::stderr_is_tty(),
            progress_len: StdCell::new(0),
        }
    }

    pub fn bold(&self, t: &str) -> String {
        self.p.bold(t)
    }
    pub fn green(&self, t: &str) -> String {
        self.p.green(t)
    }
    pub fn red(&self, t: &str) -> String {
        self.p.red(t)
    }
    pub fn yellow(&self, t: &str) -> String {
        self.p.yellow(t)
    }
    pub fn muted(&self, t: &str) -> String {
        self.p.muted(t)
    }
    pub fn accent(&self, t: &str) -> String {
        self.p.accent(t)
    }
    pub fn link(&self, t: &str) -> String {
        self.p.link(t)
    }
    /// A success verb such as "Created", in bold green.
    pub fn ok(&self, t: &str) -> String {
        self.p.strong(Tone::Green, t)
    }

    /// The command's result: JSON in --json mode, otherwise the human text.
    pub fn result(&self, data: &Value, human: impl FnOnce() -> String) {
        if self.json {
            write_out(&format!("{}\n", serde_json::to_string_pretty(data).unwrap_or_else(|_| "null".into())));
            return;
        }
        let text = human();
        if !text.is_empty() {
            write_out(&if text.ends_with('\n') { text } else { format!("{text}\n") });
        }
    }

    pub fn line(&self, text: &str) {
        if !self.json {
            write_out(&format!("{text}\n"));
        }
    }

    /// Progress and notes: stderr, so stdout stays clean for pipes. Silent in --json mode.
    pub fn note(&self, text: &str) {
        if !self.json {
            self.progress_clear();
            write_err(&format!("{text}\n"));
        }
    }

    pub fn warn(&self, text: &str) {
        self.progress_clear();
        write_err(&format!("{} {}\n", self.e.strong(Tone::Yellow, "warning:"), text));
    }

    pub fn error(&self, err: &MagineError) {
        self.progress_clear();
        if self.json {
            let mut e = serde_json::Map::new();
            e.insert("code".into(), Value::String(err.code.clone()));
            e.insert("message".into(), Value::String(err.message.clone()));
            if let Some(h) = &err.hint {
                e.insert("hint".into(), Value::String(h.clone()));
            }
            for (k, v) in &err.details {
                if !e.contains_key(k) {
                    e.insert(k.clone(), v.clone());
                }
            }
            write_err(&format!("{}\n", serde_json::json!({ "error": e })));
            return;
        }
        let mut s = format!("{} {}\n", self.e.strong(Tone::Red, "error:"), err.message);
        if let Some(h) = &err.hint {
            s.push_str(&format!("{} {}\n", self.e.strong(Tone::Link, "hint:"), self.e.muted(h)));
        }
        write_err(&s);
    }

    pub fn shows_progress(&self) -> bool {
        !self.json && self.stderr_tty
    }

    /// One line on stderr that redraws in place (only on a terminal).
    pub fn progress(&self, text: &str) {
        if !self.shows_progress() {
            return;
        }
        let len = text.chars().count();
        let pad = self.progress_len.get().saturating_sub(len);
        write_err(&format!("\r{}{}", self.e.muted(text), " ".repeat(pad)));
        self.progress_len.set(len);
    }

    pub fn progress_clear(&self) {
        let len = self.progress_len.replace(0);
        if len > 0 {
            write_err(&format!("\r{}\r", " ".repeat(len)));
        }
    }

    pub fn columns(&self) -> usize {
        term::columns()
    }
}

pub fn write_out(s: &str) {
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(s.as_bytes());
    let _ = o.flush();
}

pub fn write_err(s: &str) {
    let mut e = std::io::stderr().lock();
    let _ = e.write_all(s.as_bytes());
    let _ = e.flush();
}

/// A table cell: text, optionally coloured.
pub struct Cell {
    pub text: String,
    pub tone: Option<Tone>,
}

impl Cell {
    pub fn tone(text: impl Into<String>, tone: Tone) -> Self {
        Cell { text: text.into(), tone: Some(tone) }
    }
}

impl From<String> for Cell {
    fn from(text: String) -> Self {
        Cell { text, tone: None }
    }
}

impl From<&str> for Cell {
    fn from(text: &str) -> Self {
        Cell { text: text.to_string(), tone: None }
    }
}

/// Aligned columns with a bold header; the last column is cut to fit the terminal.
pub fn table(out: &Output, headers: &[&str], rows: Vec<Vec<Cell>>) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let n = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for r in &rows {
        for (i, c) in r.iter().enumerate().take(n) {
            widths[i] = widths[i].max(c.text.chars().count());
        }
    }
    let fixed: usize = widths[..n - 1].iter().map(|w| w + 2).sum();
    let room = out.columns().saturating_sub(fixed + 1);
    widths[n - 1] = widths[n - 1].min(room).max(10);
    let pad = |s: &str, w: usize| format!("{s}{}", " ".repeat(w.saturating_sub(s.chars().count())));
    let mut lines = Vec::with_capacity(rows.len() + 1);
    let head: Vec<String> = headers.iter().enumerate().map(|(i, h)| if i == n - 1 { h.to_string() } else { pad(h, widths[i]) }).collect();
    lines.push(out.p.strong(Tone::Accent, head.join("  ").trim_end()));
    for r in rows {
        let mut cells = Vec::with_capacity(n);
        for (i, c) in r.into_iter().enumerate().take(n) {
            let shown = if i == n - 1 { crate::util::truncate(&c.text, widths[i]) } else { pad(&c.text, widths[i]) };
            cells.push(match c.tone {
                Some(t) => {
                    let trimmed = shown.trim_end();
                    let rest = &shown[trimmed.len()..];
                    format!("{}{}", out.p.tone(t, trimmed), rest)
                }
                None => shown,
            });
        }
        lines.push(cells.join("  ").trim_end().to_string());
    }
    lines.join("\n")
}

/// `label: value` lines with the labels aligned; empty values are left out.
pub fn pairs(out: &Output, items: Vec<(&str, Option<String>)>) -> String {
    let shown: Vec<(&str, String)> = items.into_iter().filter_map(|(k, v)| v.filter(|v| !v.is_empty()).map(|v| (k, v))).collect();
    let w = shown.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
    shown
        .iter()
        .map(|(k, v)| {
            let label = format!("{k}:");
            format!("{} {}", out.muted(&format!("{label:<width$}", width = w + 1)), v)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
