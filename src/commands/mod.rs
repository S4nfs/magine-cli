pub mod account;
pub mod agents;
pub mod auth;
pub mod cattery;
pub mod explore;
pub mod guide;

use crate::errors::{usage_error, MagineError, Result};
use crate::prompt;
use crate::registry::{CommandSpec, Ctx};
use serde_json::{Map, Value};
use std::sync::OnceLock;
use std::time::Duration;

static COMMANDS: OnceLock<Vec<CommandSpec>> = OnceLock::new();

pub fn all_commands() -> &'static [CommandSpec] {
    COMMANDS.get_or_init(|| {
        let mut v = auth::commands();
        v.extend(agents::commands());
        v.extend(cattery::commands());
        v.extend(explore::commands());
        v.extend(account::wallet_commands());
        v.extend(account::key_commands());
        v.extend(guide::commands());
        v
    })
}

/// A JSON object from pairs, leaving out the ones without a value (as JSON.stringify drops undefined).
pub fn obj(pairs: Vec<(&str, Option<Value>)>) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs {
        if let Some(v) = v {
            m.insert(k.to_string(), v);
        }
    }
    Value::Object(m)
}

pub fn sv(s: Option<&str>) -> Option<Value> {
    s.map(|s| Value::String(s.to_string()))
}

/// Destructive commands ask in a terminal; elsewhere they need --yes, so nothing is deleted by accident.
pub fn confirm_or_yes(ctx: &Ctx, question: &str) -> Result<()> {
    if ctx.opts.on("yes") {
        return Ok(());
    }
    if !ctx.interactive {
        return Err(usage_error(format!("{question} Add --yes to confirm.")));
    }
    if !prompt::confirm(&format!("{} {question}", ctx.out.e.strong(crate::output::Tone::Yellow, "?")), false)? {
        return Err(MagineError::new("Cancelled.", crate::errors::exit::FAILED, "cancelled"));
    }
    Ok(())
}

/// The prompt from --prompt, --prompt-file (or - for stdin), or the words given.
pub fn prompt_from(ctx: &Ctx, words: &[String]) -> Result<String> {
    if let Some(p) = ctx.opt("prompt") {
        return Ok(p.to_string());
    }
    if let Some(file) = ctx.opt("prompt-file") {
        if file == "-" {
            return prompt::read_stdin(100 * 1024, Duration::from_secs(10));
        }
        let meta = std::fs::metadata(file).map_err(|_| usage_error(format!("No file \"{file}\".")))?;
        if meta.len() > 100 * 1024 {
            return Err(usage_error("A prompt file can be up to 100 KB."));
        }
        return std::fs::read_to_string(file).map_err(|e| usage_error(format!("Could not read \"{file}\": {e}")));
    }
    Ok(words.join(" "))
}
