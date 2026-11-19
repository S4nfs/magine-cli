use crate::api::{seg, Call};
use crate::commands::{confirm_or_yes, obj, prompt_from, sv};
use crate::editor;
use crate::errors::{exit, usage_error, MagineError, Result};
use crate::output::{pairs, table, Cell, Tone};
use crate::registry::{CommandSpec, Ctx};
use crate::tags::{self, Kind};
use crate::term;
use crate::util::{fmt_number, format_duration, format_tokens, local_time_zone, now_ms, num, opt_text, parse_duration, parse_iso_ms, rel, text, truncate};
use serde_json::{json, Value};

const AGENT: &str = "Agent name or id (\"magine agents list\" shows them)";
const AGENT_OR_QUICK: &str = "Agent name or id, or \"quick\" for quick tasks";

fn status_cell(s: &str) -> Cell {
    match s {
        "success" | "on" => Cell::tone(s, Tone::Green),
        "failed" | "error" => Cell::tone(s, Tone::Red),
        "running" | "pending" => Cell::tone(s, Tone::Yellow),
        "off" => Cell::tone(s, Tone::Muted),
        _ => Cell::from(s),
    }
}

fn print_run(ctx: &Ctx, agent_name: &str, run: &Value) -> String {
    let o = &ctx.out;
    let ok = run["status"] == "success";
    let took = if num(&run["durationMs"]) > 0.0 { format!(" in {}", format_duration(num(&run["durationMs"]))) } else { String::new() };
    let head = format!(
        "{}: {}{took}, {} tokens",
        if ok { o.ok("Done") } else { o.p.strong(Tone::Red, "Failed") },
        o.bold(agent_name),
        format_tokens(num(&run["tokensUsed"]))
    );
    let mut lines = vec![head];
    if let Some(s) = opt_text(&run["summary"]) {
        lines.push(String::new());
        lines.push(s);
    }
    if let Some(e) = opt_text(&run["error"]) {
        if Some(&e) != opt_text(&run["summary"]).as_ref() {
            lines.push(String::new());
            lines.push(o.red(&e));
        }
    }
    for img in run["images"].as_array().into_iter().flatten() {
        if let Some(u) = opt_text(&img["url"]) {
            let full = if u.starts_with('/') { format!("{}{u}", ctx.api_url) } else { u };
            lines.push(format!("Image: {}", o.link(&full)));
        }
    }
    let steps = run["steps"].as_array().map(|s| s.len()).unwrap_or(0);
    if steps > 0 {
        lines.push(String::new());
        lines.push(o.muted(&format!("{steps} step{}; add --json to see each one.", if steps == 1 { "" } else { "s" })));
    }
    lines.join("\n")
}

/// Waits for the run started at `started_at` by polling its agent's runs: short
/// requests, so neither a proxy nor an AI agent's command timeout cuts it off.
fn wait_for_run(ctx: &Ctx, agent: &str, label: &str, started_at: &str, timeout_ms: Option<u64>) -> Result<Value> {
    let api = ctx.api()?;
    let since = parse_iso_ms(started_at).unwrap_or_else(now_ms) - 2000;
    let t0 = now_ms();
    let deadline = timeout_ms.map(|t| t0 + t as i64);
    let mut delay: i64 = 2000;
    let mut saw_running = false;
    let _waiting = term::Waiting::start();
    let result = (|| -> Result<Value> {
        loop {
            let wait = match deadline {
                Some(d) => delay.min((d - now_ms()).max(0)),
                None => delay,
            };
            term::sleep_ms(wait as u64)?;
            delay = (delay + 1000).min(5000);
            let data = api.call("GET", &format!("agents/{}/runs", seg(agent)), Call::new().q("limit", Some("3")))?.data;
            let running = data["running"] == true;
            if running {
                saw_running = true;
            }
            let done = data["runs"].as_array().into_iter().flatten().find(|r| r["finishedAt"].as_str().and_then(parse_iso_ms).is_some_and(|f| f >= since));
            if let (Some(d), false) = (done, running) {
                return Ok(d.clone());
            }
            if deadline.is_some_and(|d| now_ms() >= d) {
                return Err(MagineError::new(format!("{label} is still running."), exit::WAITING, "waiting")
                    .hint(format!("Check later with: magine agents runs {agent}")));
            }
            if !saw_running && done.is_none() && now_ms() - t0 > 120_000 {
                return Err(MagineError::failed(format!("{label} did not start.")).hint(format!("See what happened with: magine agents logs {agent}")));
            }
            ctx.out.progress(&format!("{label} is running... {}", format_duration((now_ms() - t0) as f64)));
        }
    })();
    ctx.out.progress_clear();
    match result {
        Err(e) if e.exit_code == exit::INTERRUPTED => Err(MagineError::new("Stopped waiting. The run goes on on Magine.", exit::INTERRUPTED, "interrupted")
            .hint(format!("Follow it with: magine agents runs {agent}"))),
        other => other,
    }
}

fn timeout_opt(ctx: &Ctx) -> Result<Option<u64>> {
    ctx.opt("timeout").map(|t| parse_duration(t, "--timeout")).transpose()
}

/// A task typed in the terminal, with the @ and # dropdown; the tags are then checked against what it loaded.
fn ask_task(ctx: &Ctx, label: &str, hint: &str) -> Result<String> {
    let known = editor::load_in_background(ctx.api()?);
    let (task, loaded) = editor::read(label, hint, &ctx.out.e, known)?;
    if let Some((items, complete)) = &loaded {
        tags::check(ctx, &task, Some((items, *complete)));
    }
    Ok(task)
}

fn save_needs_result(ctx: &Ctx) -> Result<()> {
    if ctx.opt("save").is_some() && ctx.opts.bool("wait") == Some(false) {
        return Err(usage_error("--save keeps the result, so it needs the run to finish: leave out --no-wait."));
    }
    Ok(())
}

/// Saves the run's result to the Cattery, with the agent that made it (.md is added when the name has no extension).
fn save_result(ctx: &Ctx, agent: &str, name: &str, run: &Value) -> Result<Value> {
    let summary = opt_text(&run["summary"]).ok_or_else(|| MagineError::failed("The run gave no text to save."))?;
    let name = if name.contains('.') { name.to_string() } else { format!("{name}.md") };
    let mime = if name.ends_with(".md") { "text/markdown; charset=utf-8" } else { "text/plain; charset=utf-8" };
    let call = Call::new().q("name", Some(&name)).q("agent", Some(agent)).body(format!("{summary}\n").into_bytes(), mime);
    Ok(ctx.api()?.call("POST", "cattery", call)?.data["item"].clone())
}

/// The answer of a run or quick-task start: the result at once, a "started" note, or a wait for the result.
fn finish_run(ctx: &Ctx, status: u16, data: Value, poll_agent: &str, label: &str, title: &str, timeout: Option<u64>) -> Result<i32> {
    let o = &ctx.out;
    let (mut result, run) = if status != 202 && data["run"].is_object() {
        let run = data["run"].clone();
        (data, run)
    } else {
        if ctx.opts.bool("wait") == Some(false) {
            o.result(&data, || format!("{} {}. Follow it with: {}", o.ok("Started"), o.bold(title), o.accent(&format!("magine agents runs {poll_agent}"))));
            return Ok(exit::OK);
        }
        o.note(&format!("{} {}. {}", o.e.strong(Tone::Green, "Started"), title, o.e.muted("Ctrl+C stops waiting; the run goes on.")));
        let started_at = text(&data["startedAt"]);
        let run = wait_for_run(ctx, poll_agent, label, &started_at, timeout)?;
        (json!({ "agent": data["agent"], "startedAt": data["startedAt"], "run": run }), run)
    };
    let ok = run["status"] == "success";
    let mut saved = None;
    if let Some(name) = ctx.opt("save") {
        if ok {
            let item = save_result(ctx, poll_agent, name, &run)?;
            result["saved"] = item.clone();
            saved = Some(item);
        } else {
            o.warn("The run failed, so nothing was saved to your Cattery.");
        }
    }
    o.result(&result, || {
        let mut s = print_run(ctx, title, &run);
        if let Some(i) = &saved {
            let tag = format!("#{}", tags::file_tag_name(&text(&i["name"])));
            s.push_str(&format!(
                "\n{} to your Cattery as {}. Use it in a task as {}.",
                o.ok("Saved"),
                o.p.strong(Tone::Accent, &text(&i["name"])),
                o.accent(&tag)
            ));
        }
        s
    });
    Ok(if ok { exit::OK } else { exit::FAILED })
}

fn list(ctx: &Ctx) -> Result<i32> {
    let data = ctx.api()?.get("agents")?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let agents = data["agents"].as_array().cloned().unwrap_or_default();
        if agents.is_empty() {
            return format!("No agents yet. Create one: {}", o.accent("magine agents create \"what it should do\""));
        }
        table(
            o,
            &["NAME", "MODE", "STATUS", "SCHEDULE", "LAST RUN"],
            agents
                .iter()
                .map(|a| {
                    vec![
                        Cell::tone(text(&a["name"]), Tone::Accent),
                        Cell::from(text(&a["mode"])),
                        status_cell(if a["active"] == true { "on" } else { "off" }),
                        Cell::from(opt_text(&a["schedule"]).unwrap_or_else(|| "-".into())),
                        Cell::from(rel(&a["lastRunAt"])),
                    ]
                })
                .collect(),
        )
    });
    Ok(exit::OK)
}

fn show(ctx: &Ctx) -> Result<i32> {
    let data = ctx.api()?.get(&format!("agents/{}", seg(&ctx.args[0])))?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let a = &data["agent"];
        let schedule = opt_text(&a["schedule"]).map(|s| match opt_text(&a["cron"]) {
            Some(c) if c != s => format!("{s} ({c})"),
            _ => s,
        });
        let watches =
            if a["heartbeat"].is_object() { Some(format!("{} -> {}", text(&a["heartbeat"]["watch"]), text(&a["heartbeat"]["action"]))) } else { None };
        let from = if a["marketplace"].is_object() { Some(format!("marketplace: {}", text(&a["marketplace"]["title"]))) } else { None };
        let mode = match opt_text(&a["workflow"]) {
            Some(w) => format!("{} ({w})", text(&a["mode"])),
            None => text(&a["mode"]),
        };
        let status = format!(
            "{}{}",
            if a["active"] == true { o.green("on") } else { o.muted("off") },
            if a["running"] == true { format!(", {}", o.yellow("running now")) } else { String::new() }
        );
        [
            pairs(
                o,
                vec![
                    ("Name", Some(o.p.strong(Tone::Accent, &text(&a["name"])))),
                    ("Id", opt_text(&a["id"])),
                    ("Mode", Some(mode)),
                    ("Status", Some(status)),
                    ("Schedule", schedule),
                    ("Watches", watches),
                    ("From", from),
                    ("Last run", Some(rel(&a["lastRunAt"]))),
                    ("Created", Some(rel(&a["createdAt"]))),
                ],
            ),
            String::new(),
            text(&a["prompt"]),
        ]
        .join("\n")
    });
    Ok(exit::OK)
}

fn create(ctx: &Ctx) -> Result<i32> {
    let mut prompt = prompt_from(ctx, &ctx.args)?.trim().to_string();
    let mode = ctx.opt("mode").unwrap_or("agent");
    if prompt.is_empty() && mode != "heartbeat" && ctx.interactive {
        prompt = ask_task(ctx, "What should the agent do?", "Type @ to tag an agent, # for a Cattery file. Enter creates it (1 Cat).")?;
    } else {
        tags::check(ctx, &prompt, None);
    }
    if !["agent", "sda", "heartbeat"].contains(&mode) {
        return Err(usage_error("--mode is agent, sda or heartbeat."));
    }
    if mode != "heartbeat" && prompt.chars().count() < 3 {
        return Err(usage_error("Say what the agent should do.").hint("magine agents create \"what it should do\""));
    }
    let heartbeat = if mode == "heartbeat" {
        if ctx.opt("watch").is_none() || ctx.opt("action").is_none() {
            return Err(usage_error("A heartbeat agent needs --watch and --action."));
        }
        let interval = ctx.opt("interval").map(|i| parse_duration(i, "--interval")).transpose()?;
        Some(obj(vec![
            ("watch", sv(ctx.opt("watch"))),
            ("action", sv(ctx.opt("action"))),
            ("intervalMs", interval.map(|n| json!(n))),
            ("targetUrl", sv(ctx.opt("url"))),
        ]))
    } else {
        None
    };
    let tz = ctx.opt("timezone").map(str::to_string).or_else(|| local_time_zone(&ctx.env));
    let body = obj(vec![
        ("prompt", Some(json!(prompt))),
        ("mode", Some(json!(mode))),
        ("name", sv(ctx.opt("name"))),
        ("workflow", sv(ctx.opt("workflow"))),
        ("schedule", sv(ctx.opt("schedule"))),
        ("timezone", tz.map(Value::String)),
        ("heartbeat", heartbeat),
    ]);
    let data = ctx.api()?.call("POST", "agents", Call::new().json(body))?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let a = &data["agent"];
        let cats = num(&data["catsCharged"]);
        let mut lines = vec![format!(
            "{} {} ({} Cat{}).",
            o.ok("Created"),
            o.p.strong(Tone::Accent, &text(&a["name"])),
            fmt_number(cats),
            if cats == 1.0 { "" } else { "s" }
        )];
        if data["schedule"].is_object() {
            let s = &data["schedule"];
            let label = opt_text(&s["label"]).unwrap_or_else(|| text(&s["schedule"]));
            let cron = opt_text(&s["cron"]).map(|c| format!(" ({c})")).unwrap_or_default();
            lines.push(format!("Schedule: {label}{cron}."));
            if s["needsEnable"] == true {
                lines.push(format!("It is off until you run it once or turn it on: {}", o.accent(&format!("magine agents enable {}", text(&a["name"])))));
            }
        }
        lines.push(format!("Run it now: {}", o.accent(&format!("magine agents run {}", text(&a["name"])))));
        lines.join("\n")
    });
    Ok(exit::OK)
}

fn edit(ctx: &Ctx) -> Result<i32> {
    let mut body = serde_json::Map::new();
    if ctx.opt("prompt").is_some() || ctx.opt("prompt-file").is_some() {
        let prompt = prompt_from(ctx, &[])?.trim().to_string();
        tags::check(ctx, &prompt, None);
        body.insert("prompt".into(), json!(prompt));
    }
    if let Some(n) = ctx.opt("name") {
        body.insert("name".into(), json!(n));
    }
    if let Some(w) = ctx.opt("workflow") {
        body.insert("workflow".into(), json!(w));
    }
    if body.is_empty() {
        return Err(usage_error("Nothing to change.").hint("Give --prompt, --prompt-file, --name or --workflow."));
    }
    let data = ctx.api()?.call("PATCH", &format!("agents/{}", seg(&ctx.args[0])), Call::new().json(Value::Object(body)))?.data;
    let o = &ctx.out;
    o.result(&data, || format!("{} {}.", o.ok("Updated"), o.p.strong(Tone::Accent, &text(&data["agent"]["name"]))));
    Ok(exit::OK)
}

fn set_active(ctx: &Ctx, active: bool) -> Result<i32> {
    let data = ctx.api()?.call("PATCH", &format!("agents/{}", seg(&ctx.args[0])), Call::new().json(json!({ "active": active })))?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let on = data["agent"]["active"] == true;
        format!("{} is {}.", o.p.strong(Tone::Accent, &text(&data["agent"]["name"])), if on { o.ok("on") } else { o.muted("off") })
    });
    Ok(exit::OK)
}

fn enable(ctx: &Ctx) -> Result<i32> {
    set_active(ctx, true)
}

fn disable(ctx: &Ctx) -> Result<i32> {
    set_active(ctx, false)
}

fn delete(ctx: &Ctx) -> Result<i32> {
    confirm_or_yes(ctx, &format!("Delete the agent \"{}\" with its runs and files?", ctx.args[0]))?;
    let data = ctx.api()?.call("DELETE", &format!("agents/{}", seg(&ctx.args[0])), Call::new())?.data;
    let o = &ctx.out;
    o.result(&data, || format!("{} {}.", o.ok("Deleted"), text(&data["name"])));
    Ok(exit::OK)
}

fn run(ctx: &Ctx) -> Result<i32> {
    let agent = &ctx.args[0];
    let timeout = timeout_opt(ctx)?;
    save_needs_result(ctx)?;
    if let Some(task) = ctx.opt("task") {
        tags::check(ctx, task, None);
    }
    let r = ctx.api()?.call(
        "POST",
        &format!("agents/{}/run", seg(agent)),
        Call::new().json(obj(vec![("task", sv(ctx.opt("task"))), ("wait", Some(json!(false)))])),
    )?;
    let name = opt_text(&r.data["agent"]["name"]).unwrap_or_else(|| agent.clone());
    finish_run(ctx, r.status, r.data.clone(), &name, &name, &name, timeout)
}

fn stop(ctx: &Ctx) -> Result<i32> {
    let data = ctx.api()?.call("POST", &format!("agents/{}/stop", seg(&ctx.args[0])), Call::new())?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let stopped: Vec<String> = data["stopped"].as_array().into_iter().flatten().map(text).collect();
        let name = text(&data["agent"]["name"]);
        if !stopped.is_empty() {
            format!("{} {}: {}.", o.ok("Stopped"), o.bold(&name), stopped.join(", "))
        } else {
            opt_text(&data["note"]).unwrap_or_else(|| format!("{name} was not running."))
        }
    });
    Ok(exit::OK)
}

fn runs(ctx: &Ctx) -> Result<i32> {
    let call = Call::new().q("limit", ctx.opt("limit")).q("page", ctx.opt("page")).q("q", ctx.opt("search"));
    let data = ctx.api()?.call("GET", &format!("agents/{}/runs", seg(&ctx.args[0])), call)?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let head = if data["running"] == true { format!("{}\n", o.p.strong(Tone::Yellow, "Running now.")) } else { String::new() };
        let runs = data["runs"].as_array().cloned().unwrap_or_default();
        if runs.is_empty() {
            return format!("{head}No runs yet.");
        }
        head + &table(
            o,
            &["FINISHED", "STATUS", "TOKENS", "SUMMARY"],
            runs.iter()
                .map(|r| {
                    let summary = opt_text(&r["summary"]).or_else(|| opt_text(&r["error"])).unwrap_or_default();
                    vec![
                        Cell::from(rel(&r["finishedAt"])),
                        status_cell(&text(&r["status"])),
                        Cell::from(format_tokens(num(&r["tokensUsed"]))),
                        Cell::from(truncate(&summary, 400)),
                    ]
                })
                .collect(),
        )
    });
    Ok(exit::OK)
}

fn logs(ctx: &Ctx) -> Result<i32> {
    let call = Call::new().q("limit", ctx.opt("limit")).q("page", ctx.opt("page"));
    let data = ctx.api()?.call("GET", &format!("agents/{}/logs", seg(&ctx.args[0])), call)?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let logs = data["logs"].as_array().cloned().unwrap_or_default();
        if logs.is_empty() {
            return "Nothing logged yet.".into();
        }
        table(
            o,
            &["WHEN", "ACTION", "STATUS", "DETAILS"],
            logs.iter()
                .map(|l| {
                    vec![
                        Cell::from(rel(&l["at"])),
                        Cell::from(text(&l["action"])),
                        status_cell(&text(&l["status"])),
                        Cell::from(truncate(&text(&l["details"]), 300)),
                    ]
                })
                .collect(),
        )
    });
    Ok(exit::OK)
}

fn schedule(ctx: &Ctx) -> Result<i32> {
    let agent = &ctx.args[0];
    let when = ctx.args[1..].join(" ");
    let api = ctx.api()?;
    let o = &ctx.out;
    if ctx.opts.on("clear") {
        let data = api.call("DELETE", &format!("agents/{}/schedule", seg(agent)), Call::new())?.data;
        o.result(&data, || format!("{} has no schedule now.", o.bold(&text(&data["agent"]["name"]))));
        return Ok(exit::OK);
    }
    if when.trim().is_empty() {
        return Err(usage_error("Say when it should run.").hint("magine agents schedule <agent> every day at 9am   (or --clear)"));
    }
    let tz = ctx.opt("timezone").map(str::to_string).or_else(|| local_time_zone(&ctx.env));
    let data = api
        .call(
            "PUT",
            &format!("agents/{}/schedule", seg(agent)),
            Call::new().json(obj(vec![("schedule", Some(json!(when))), ("timezone", tz.map(Value::String))])),
        )?
        .data;
    o.result(&data, || {
        let name = text(&data["agent"]["name"]);
        let label = opt_text(&data["label"]).unwrap_or_else(|| text(&data["schedule"]));
        let cron = opt_text(&data["cron"]).map(|c| format!(" ({c})")).unwrap_or_default();
        let mut lines = vec![format!("{} {} runs {label}{cron}.", o.ok("Scheduled:"), o.bold(&name))];
        if data["needsEnable"] == true {
            lines.push(format!("It is off: turn it on with {}.", o.accent(&format!("magine agents enable {name}"))));
        }
        lines.join("\n")
    });
    Ok(exit::OK)
}

fn quick_task(ctx: &Ctx) -> Result<i32> {
    let mut prompt = ctx.args.join(" ").trim().to_string();
    let timeout = timeout_opt(ctx)?;
    save_needs_result(ctx)?;
    if prompt.is_empty() {
        if !ctx.interactive {
            return Err(usage_error("Say what to do.").hint("magine do \"what to do\"   (tag agents with @name and Cattery files with #name)"));
        }
        prompt = ask_task(ctx, "Task:", "Type @ to tag an agent, # for a Cattery file. Enter runs it, Ctrl+C cancels.")?;
    } else {
        tags::check(ctx, &prompt, None);
    }
    if prompt.chars().count() < 3 {
        return Err(usage_error("Say what to do (at least 3 characters)."));
    }
    let r = ctx.api()?.call("POST", "do", Call::new().json(json!({ "prompt": prompt, "wait": false })))?;
    finish_run(ctx, r.status, r.data.clone(), "quick", "The quick task", "Quick task", timeout)
}

fn list_tags(ctx: &Ctx) -> Result<i32> {
    let raw = ctx.args.first().map(String::as_str).unwrap_or("").trim();
    let (only, query) = match raw.chars().next() {
        Some('@') => (Some(Kind::Agent), &raw[1..]),
        Some('#') => (Some(Kind::File), &raw[1..]),
        _ => (None, raw),
    };
    let only = only.or(if ctx.opts.on("agents") && !ctx.opts.on("files") {
        Some(Kind::Agent)
    } else if ctx.opts.on("files") && !ctx.opts.on("agents") {
        Some(Kind::File)
    } else {
        None
    });
    let limit = match ctx.opt("limit") {
        Some(l) => l.parse::<usize>().ok().filter(|n| *n > 0).ok_or_else(|| usage_error("--limit is a whole number above 0."))?,
        None => 20,
    };
    let (items, complete) = tags::load(&ctx.api()?, only != Some(Kind::File), only != Some(Kind::Agent))?;
    let mut picked = Vec::new();
    for kind in [Kind::Agent, Kind::File] {
        if only.is_none_or(|k| k == kind) {
            picked.extend(tags::suggest(&items, kind, query, limit));
        }
    }
    let data = json!({
        "tags": picked.iter().map(|t| json!({ "tag": t.full(), "type": t.kind.label(), "name": t.name, "id": t.id, "detail": t.detail })).collect::<Vec<_>>(),
        "filesComplete": complete,
    });
    let o = &ctx.out;
    o.result(&data, || {
        if picked.is_empty() {
            return if raw.is_empty() {
                format!(
                    "Nothing to tag yet. Create an agent ({}) or add a file ({}).",
                    o.accent("magine agents create"),
                    o.accent("magine cattery upload <file>")
                )
            } else {
                format!("Nothing matches \"{raw}\".")
            };
        }
        let rows = picked.iter().map(|t| vec![Cell::tone(t.full(), Tone::Accent), Cell::from(t.kind.label()), Cell::from(truncate(&t.detail, 200))]).collect();
        format!("{}\n\n{}", table(o, &["TAG", "TYPE", "DETAILS"], rows), o.muted("Use them in a task: magine do \"Summarise #report.pdf like @news-bot does\""))
    });
    Ok(exit::OK)
}

pub fn commands() -> Vec<CommandSpec> {
    let wait = "Wait for the result (default; --no-wait returns once it starts)";
    let timeout = "Stop waiting after this long (e.g. 10m); the run goes on";
    let save = "Save the result to your Cattery under this name (.md is added without an extension)";
    vec![
        CommandSpec::new("agents list", "Agents", "List your agents", list).aliases(&["agents", "agents ls"]),
        CommandSpec::new("agents show", "Agents", "Show one agent", show).aliases(&["agents get"]).arg("agent", AGENT, true, false),
        CommandSpec::new("agents create", "Agents", "Create an agent", create)
            .aliases(&["agents new"])
            .costs("1 Cat ($5); a heartbeat agent 2 Cats")
            .desc("Creates an agent from a description of what it should do. It runs when you run it, on its schedule, or (heartbeat) when what it watches changes. In a terminal, leave out the prompt to type it with suggestions for @agents and #files.")
            .arg("prompt", "What the agent should do (or use --prompt / --prompt-file)", false, true)
            .value("prompt", "text", "What the agent should do")
            .value("prompt-file", "path", "Read the prompt from a file (- for stdin)")
            .value("name", "slug", "Name (lowercase letters, digits, hyphens); made up from the prompt if left out")
            .value("mode", "agent|sda|heartbeat", "agent (default), sda (sees the screen) or heartbeat (watches and reacts)")
            .value("workflow", "classic|cdp|agp", "How it drives the browser (default: the platform choice)")
            .value("schedule", "when", "e.g. \"every day at 9am\", \"every 30 minutes\", \"0 9 * * *\"")
            .value("timezone", "zone", "Time zone for the schedule (default: this computer's)")
            .value("watch", "text", "Heartbeat: what to watch for")
            .value("action", "text", "Heartbeat: what to do when it happens")
            .value("interval", "duration", "Heartbeat: how often to look (e.g. 30s, 5m)")
            .value("url", "url", "Heartbeat: the page to watch")
            .examples(&[
                "magine agents create \"Every morning, list the top 10 Hacker News stories\" --schedule \"every day at 8am\"",
                "magine agents create --mode heartbeat --watch \"a new message from my boss\" --action \"summarise it\" --url https://mail.google.com",
            ]),
        CommandSpec::new("agents edit", "Agents", "Change an agent's prompt, name or workflow", edit)
            .aliases(&["agents update"])
            .arg("agent", AGENT, true, false)
            .value("prompt", "text", "New prompt")
            .value("prompt-file", "path", "Read the new prompt from a file (- for stdin)")
            .value("name", "slug", "New name")
            .value("workflow", "classic|cdp|agp", "How it drives the browser"),
        CommandSpec::new("agents enable", "Agents", "Turn an agent on (its schedule or watcher starts)", enable).arg("agent", AGENT, true, false),
        CommandSpec::new("agents disable", "Agents", "Turn an agent off (no scheduled runs)", disable).arg("agent", AGENT, true, false),
        CommandSpec::new("agents delete", "Agents", "Delete an agent and its files", delete).aliases(&["agents rm"]).arg("agent", AGENT, true, false).flag_alias("yes", "y", "Do not ask"),
        CommandSpec::new("agents run", "Agents", "Run an agent now and wait for the result", run)
            .costs("tokens for each step it takes")
            .desc("Runs the agent now. Running an agent by hand also switches it on, as on the website, so its schedule starts.")
            .arg("agent", AGENT, true, false)
            .value("task", "text", "Extra instructions for this run only (tag @agents and #files)")
            .flag("wait", wait)
            .value("timeout", "duration", timeout)
            .value("save", "name", save),
        CommandSpec::new("agents stop", "Agents", "Stop an agent: its run, schedule and watcher", stop).arg("agent", "Agent name or id, or \"quick\" for the quick task", true, false),
        CommandSpec::new("agents runs", "Agents", "Show an agent's latest runs and results", runs)
            .aliases(&["agents results"])
            .arg("agent", AGENT_OR_QUICK, true, false)
            .value("limit", "n", "How many (default 10, max 50)")
            .value("page", "n", "Page, from 0")
            .value("search", "text", "Only runs mentioning this"),
        CommandSpec::new("agents logs", "Agents", "Show an agent's activity log", logs)
            .arg("agent", AGENT_OR_QUICK, true, false)
            .value("limit", "n", "How many (default 30, max 100)")
            .value("page", "n", "Page, from 0"),
        CommandSpec::new("agents schedule", "Agents", "Set or clear when an agent runs", schedule)
            .arg("agent", AGENT, true, false)
            .arg("when", "e.g. every day at 9am, every 30 minutes, 0 9 * * *", false, true)
            .value("timezone", "zone", "Time zone (default: this computer's)")
            .flag("clear", "Remove the schedule"),
        CommandSpec::new("do", "Agents", "Run a one-off task without creating an agent", quick_task)
            .costs("tokens for each step it takes (no Cat needed)")
            .desc("A quick task: an agent that does this once and is not saved. Tag your agents with @name (their latest result comes along) and Cattery files with #name (the agent gets the file); \"magine tags\" lists the names. In a terminal, leave out the task to type it with suggestions.")
            .arg("task", "What to do", false, true)
            .flag("wait", wait)
            .value("timeout", "duration", timeout)
            .value("save", "name", save)
            .examples(&[
                "magine do \"Find the 3 cheapest flights from San Francisco to New York next Friday\"",
                "magine do \"Summarise #report.pdf in 5 bullet points\" --save summary.md",
                "magine do \"Compare today's prices with what @price-watch found\" --json",
            ]),
        CommandSpec::new("tags", "Agents", "List what a task can tag: @agents and #files", list_tags)
            .aliases(&["tag"])
            .desc("A task can tag your agents with @name and your Cattery files with #name. This lists the exact names, best matches first; start the query with @ or # to pick the kind.")
            .arg("query", "Part of a name, e.g. @news or #report", false, false)
            .flag("agents", "Only agents")
            .flag("files", "Only Cattery files")
            .value("limit", "n", "How many of each kind (default 20)")
            .examples(&["magine tags", "magine tags @news", "magine tags \"#report\" --json"]),
    ]
}
