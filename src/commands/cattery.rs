use crate::api::{seg, Api, Call};
use crate::commands::confirm_or_yes;
use crate::errors::{exit, usage_error, MagineError, Result};
use crate::output::{pairs, table, Cell, Tone};
use crate::prompt;
use crate::registry::{CommandSpec, Ctx};
use crate::tags::{self, Kind};
use crate::term;
use crate::util::{format_bytes, num, opt_text, rel, text, truncate};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_UPLOAD: u64 = 50 * 1024 * 1024;
const ID: &str = "File id or #name (\"magine cattery list\" shows them)";

/// The id of a file given by id, or by #name or file name (the newest file with that name).
fn file_id(api: &Api, arg: &str) -> Result<String> {
    let name = arg.strip_prefix('#').unwrap_or(arg);
    if !arg.starts_with('#') && !arg.contains('.') {
        return Ok(arg.to_string());
    }
    let (items, _) = tags::load(api, false, true)?;
    items
        .iter()
        .find(|t| tags::matches(t, Kind::File, name))
        .map(|t| t.id.clone())
        .ok_or_else(|| MagineError::new(format!("No Cattery file called #{name}."), exit::NOT_FOUND, "not_found").hint("See the names with: magine tags \"#\""))
}

fn windows_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || ((stem.starts_with("com") || stem.starts_with("lpt")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit())
}

/// A server-given file name made safe to create here: no folders, no characters Windows refuses.
pub fn safe_file_name(name: &str) -> String {
    let unified = name.replace('\\', "/");
    let base = unified.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
    let replaced: String = base.chars().map(|c| if "<>:\"/\\|?*".contains(c) || (c as u32) < 0x20 { '_' } else { c }).collect();
    let mut n = replaced.trim_end_matches(['.', ' ']).trim().to_string();
    if n.is_empty() || n == "." || n == ".." {
        n = "download".into();
    }
    if windows_reserved(&n) {
        n = format!("_{n}");
    }
    n.chars().take(200).collect()
}

fn list(ctx: &Ctx) -> Result<i32> {
    let call = Call::new().q("kind", ctx.opt("kind")).q("agent", ctx.opt("agent")).q("limit", ctx.opt("limit")).q("page", ctx.opt("page"));
    let data = ctx.api()?.call("GET", "cattery", call)?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let items = data["items"].as_array().cloned().unwrap_or_default();
        if items.is_empty() {
            return format!("Your Cattery is empty. Add a file: {}", o.accent("magine cattery upload <file>"));
        }
        let mut s = table(
            o,
            &["ID", "KIND", "SIZE", "ADDED", "AGENT", "NAME"],
            items
                .iter()
                .map(|i| {
                    vec![
                        Cell::tone(text(&i["id"]), Tone::Muted),
                        Cell::from(text(&i["kind"])),
                        Cell::from(format_bytes(num(&i["bytes"]))),
                        Cell::from(rel(&i["createdAt"])),
                        Cell::from(truncate(&opt_text(&i["agentName"]).unwrap_or_else(|| "-".into()), 24)),
                        Cell::tone(text(&i["name"]), Tone::Accent),
                    ]
                })
                .collect(),
        );
        if !data["nextPage"].is_null() {
            s.push_str(&format!("\n{}", o.muted(&format!("More: --page {}", text(&data["nextPage"])))));
        }
        s
    });
    Ok(exit::OK)
}

fn show(ctx: &Ctx) -> Result<i32> {
    let api = ctx.api()?;
    let id = file_id(&api, &ctx.args[0])?;
    let data = api.get(&format!("cattery/{}", seg(&id)))?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let i = &data["item"];
        pairs(
            o,
            vec![
                ("Name", Some(o.p.strong(Tone::Accent, &text(&i["name"])))),
                ("Tag", Some(o.accent(&format!("#{}", tags::file_tag_name(&text(&i["name"])))))),
                ("Id", opt_text(&i["id"])),
                ("Kind", Some(format!("{} ({})", text(&i["kind"]), text(&i["mimeType"])))),
                ("Size", Some(format_bytes(num(&i["bytes"])))),
                ("Source", opt_text(&i["source"])),
                ("Agent", opt_text(&i["agentName"]).or_else(|| opt_text(&i["agentId"]))),
                ("Prompt", opt_text(&i["prompt"])),
                ("Added", Some(rel(&i["createdAt"]))),
            ],
        )
    });
    Ok(exit::OK)
}

fn part_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_os_string();
    s.push(format!(".magine-{}.part", std::process::id()));
    PathBuf::from(s)
}

#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new().write(true).create(true).truncate(true).open(path)
}

fn download(ctx: &Ctx) -> Result<i32> {
    let api = ctx.api()?;
    let id = &file_id(&api, &ctx.args[0])?;
    let meta = api.get(&format!("cattery/{}", seg(id)))?.data;
    let item = &meta["item"];
    let content = format!("cattery/{}/content", seg(id));
    let out = ctx.opt("output").unwrap_or("");
    if out == "-" {
        if ctx.out.json {
            return Err(usage_error("--output - writes the file itself to stdout; leave out --json."));
        }
        let mut stdout = std::io::stdout().lock();
        api.call("GET", &content, Call::new().sink(&mut stdout).timeout_ms(300_000))?;
        return Ok(exit::OK);
    }
    let name = safe_file_name(&text(&item["name"]));
    let cwd = std::env::current_dir().map_err(|e| MagineError::failed(format!("No current folder: {e}")))?;
    let mut target = if out.is_empty() { cwd.join(&name) } else { cwd.join(out) };
    if !out.is_empty() && target.is_dir() {
        target = target.join(&name);
    }
    if target.exists() && !ctx.opts.on("force") {
        return Err(MagineError::new(format!("{} already exists.", target.display()), exit::CONFLICT, "exists")
            .hint("Add --force to overwrite it, or choose another path with -o."));
    }
    let parent = target.parent().map(Path::to_path_buf).unwrap_or_else(|| cwd.clone());
    if !parent.is_dir() {
        return Err(usage_error(format!("The folder {} does not exist.", parent.display())));
    }
    let tmp = part_path(&target);
    let mut file = create_private(&tmp).map_err(|e| MagineError::failed(format!("Could not create {}: {e}", tmp.display())))?;
    let result = api.call("GET", &content, Call::new().sink(&mut file).timeout_ms(300_000).max_bytes(200 * 1024 * 1024));
    drop(file);
    let finish = || -> Result<()> {
        result?;
        let size = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
        let expected = num(&item["bytes"]) as u64;
        if expected > 0 && size != expected {
            return Err(MagineError::network(format!("The download is incomplete ({size} of {expected} bytes). Try again.")).details(Default::default()));
        }
        fs::rename(&tmp, &target).map_err(|e| MagineError::failed(format!("Could not save {}: {e}", target.display())))
    };
    if let Err(e) = finish() {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    let path = target.display().to_string();
    let o = &ctx.out;
    o.result(&json!({ "id": item["id"], "name": item["name"], "path": path, "bytes": item["bytes"] }), || {
        format!("{} {} ({}).", o.ok("Saved"), o.link(&path), format_bytes(num(&item["bytes"])))
    });
    Ok(exit::OK)
}

fn upload(ctx: &Ctx) -> Result<i32> {
    let file = &ctx.args[0];
    let (name, bytes) = if file == "-" {
        let Some(name) = ctx.opt("name") else {
            return Err(usage_error("Name the file with --name when it comes from stdin.").hint("magine cattery upload - --name notes.md < notes.md"));
        };
        if term::stdin_is_tty() {
            return Err(usage_error("Pipe the file in: magine cattery upload - --name notes.md < notes.md"));
        }
        let bytes = prompt::read_stdin_bytes(MAX_UPLOAD as usize, Duration::from_secs(10))?;
        if bytes.is_empty() {
            return Err(usage_error("Nothing arrived on stdin."));
        }
        (name.to_string(), bytes)
    } else {
        let meta = fs::metadata(file).ok().filter(|m| m.is_file()).ok_or_else(|| usage_error(format!("No file \"{file}\".")))?;
        if meta.len() == 0 {
            return Err(usage_error("The file is empty."));
        }
        if meta.len() > MAX_UPLOAD {
            return Err(MagineError::new(format!("{file} is {}; the limit is 50 MB.", format_bytes(meta.len() as f64)), exit::FAILED, "too_large"));
        }
        let name = ctx
            .opt("name")
            .map(str::to_string)
            .unwrap_or_else(|| Path::new(file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| file.clone()));
        (name, fs::read(file).map_err(|e| usage_error(format!("Could not read \"{file}\": {e}")))?)
    };
    let call = Call::new().q("name", Some(&name)).q("agent", ctx.opt("agent")).body(bytes, "application/octet-stream").timeout_ms(300_000);
    let data = ctx.api()?.call("POST", "cattery", call)?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let i = &data["item"];
        format!(
            "{} {} ({}), id {}. Tag it in a task as {}",
            o.ok("Uploaded"),
            o.p.strong(Tone::Accent, &text(&i["name"])),
            format_bytes(num(&i["bytes"])),
            o.muted(&text(&i["id"])),
            o.accent(&format!("#{}", tags::file_tag_name(&text(&i["name"]))))
        )
    });
    Ok(exit::OK)
}

fn delete(ctx: &Ctx) -> Result<i32> {
    let api = ctx.api()?;
    let id = file_id(&api, &ctx.args[0])?;
    confirm_or_yes(ctx, &format!("Delete the Cattery file {}?", ctx.args[0]))?;
    let data = api.call("DELETE", &format!("cattery/{}", seg(&id)), Call::new())?.data;
    let o = &ctx.out;
    o.result(&data, || format!("{} {}.", o.ok("Deleted"), text(&data["id"])));
    Ok(exit::OK)
}

pub fn commands() -> Vec<CommandSpec> {
    let g = "Cattery (your files)";
    vec![
        CommandSpec::new("cattery list", g, "List your Cattery files", list)
            .aliases(&["cattery", "cattery ls"])
            .value("kind", "image|pdf|text|binary", "Only this kind")
            .value("agent", "agent", "Only files of this agent (\"quick\" for quick tasks)")
            .value("limit", "n", "How many (default 24, max 60)")
            .value("page", "n", "Page, from 0"),
        CommandSpec::new("cattery show", g, "Show a file's details", show).aliases(&["cattery info"]).arg("id", ID, true, false),
        CommandSpec::new("cattery download", g, "Download a file", download)
            .aliases(&["cattery get"])
            .arg("id", ID, true, false)
            .value_alias("output", "o", "path", "Where to save it (a folder or file; - for stdout). Default: its name, here")
            .flag_alias("force", "f", "Overwrite an existing file"),
        CommandSpec::new("cattery upload", g, "Upload a file (up to 50 MB) for your agents to use", upload)
            .aliases(&["cattery add"])
            .desc("Uploads a file to your Cattery. Agents can then use it when a task tags it, e.g. \"summarise #report.pdf\". Use - to read it from stdin.")
            .arg("file", "Path of the file, or - for stdin (with --name)", true, false)
            .value("name", "name", "Name in the Cattery (default: the file name)")
            .value("agent", "agent", "Keep it with this agent")
            .examples(&["magine cattery upload report.pdf", "magine agents logs news-bot --json | magine cattery upload - --name news-log.json"]),
        CommandSpec::new("cattery delete", g, "Delete a file", delete).aliases(&["cattery rm"]).arg("id", ID, true, false).flag_alias("yes", "y", "Do not ask"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_from_the_server_are_safe_on_every_os() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("..\\..\\Windows\\win.ini"), "win.ini");
        assert_eq!(safe_file_name("a<b>c:d\"e|f?g*h.txt"), "a_b_c_d_e_f_g_h.txt");
        assert_eq!(safe_file_name("CON"), "_CON");
        assert_eq!(safe_file_name("nul.txt"), "_nul.txt");
        assert_eq!(safe_file_name("com1"), "_com1");
        assert_eq!(safe_file_name("report. "), "report");
        assert_eq!(safe_file_name(""), "download");
        assert_eq!(safe_file_name(".."), "download");
        assert_eq!(safe_file_name(&"x".repeat(300)).chars().count(), 200);
        assert_eq!(safe_file_name("dir/"), "dir");
    }
}
