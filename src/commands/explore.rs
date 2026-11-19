use crate::api::{seg, Call};
use crate::errors::{exit, usage_error, Result};
use crate::output::{pairs, table, Cell, Tone};
use crate::registry::{CommandSpec, Ctx};
use crate::util::{fmt_number, format_tokens, group_thousands, num, opt_text, text, truncate};
use serde_json::Value;

const NEURON_KINDS: [&str; 10] = ["agent", "site", "memory", "lesson", "category", "fact", "identity", "person", "meeting", "bookmark"];
const KINDS_PLACEHOLDER: &str = "agent|site|memory|lesson|category|fact|identity|person|meeting|bookmark";
const GROUP: &str = "Neurons, marketplace, GitHub";

fn cats(n: f64) -> String {
    format!("{} Cat{}", fmt_number(n), if n == 1.0 { "" } else { "s" })
}

fn template_rows(templates: &[Value]) -> Vec<Vec<Cell>> {
    templates
        .iter()
        .map(|t| {
            vec![
                Cell::tone(text(&t["id"]), Tone::Muted),
                Cell::tone(cats(num(&t["deployCostCats"])), Tone::Yellow),
                Cell::from(text(&t["installs"])),
                Cell::from(text(&t["mode"])),
                Cell::tone(truncate(&text(&t["title"]), 60), Tone::Accent),
            ]
        })
        .collect()
}

fn neurons(ctx: &Ctx) -> Result<i32> {
    if let Some(k) = ctx.opt("kind") {
        if !NEURON_KINDS.contains(&k) {
            return Err(usage_error(format!("--kind is one of: {}.", NEURON_KINDS.join(", "))));
        }
    }
    let q = ctx.args.join(" ");
    let call = Call::new().q("q", Some(&q)).q("kind", ctx.opt("kind")).q("limit", ctx.opt("limit"));
    let data = ctx.api()?.call("GET", "neurons", call)?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let totals = data["totals"]["byKind"]
            .as_object()
            .map(|m| m.iter().map(|(k, n)| format!("{} {k}{}", text(n), if num(n) == 1.0 { "" } else { "s" })).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let head = o.muted(&format!(
            "Neurons: {} memories, {} links{}",
            opt_text(&data["totals"]["nodes"]).unwrap_or_else(|| "0".into()),
            opt_text(&data["totals"]["links"]).unwrap_or_else(|| "0".into()),
            if totals.is_empty() { String::new() } else { format!(" ({totals})") }
        ));
        let nodes = data["nodes"].as_array().cloned().unwrap_or_default();
        if nodes.is_empty() {
            return format!("{head}\nNothing matches.");
        }
        format!(
            "{head}\n{}",
            table(
                o,
                &["KIND", "WEIGHT", "LABEL"],
                nodes
                    .iter()
                    .map(|n| vec![
                        Cell::tone(text(&n["kind"]), Tone::Cat),
                        Cell::from(format!("{:.2}", num(&n["weight"]))),
                        Cell::from(truncate(&text(&n["label"]), 200))
                    ])
                    .collect()
            )
        )
    });
    Ok(exit::OK)
}

fn more(ctx: &Ctx, data: &Value) -> String {
    if data["nextPage"].is_null() {
        String::new()
    } else {
        format!("\n{}", ctx.out.muted(&format!("More: --page {}", text(&data["nextPage"]))))
    }
}

fn market_list(ctx: &Ctx) -> Result<i32> {
    let call = Call::new().q("sort", ctx.opt("sort")).q("limit", ctx.opt("limit")).q("page", ctx.opt("page"));
    let data = ctx.api()?.call("GET", "marketplace", call)?.data;
    let o = &ctx.out;
    o.result(&data, || {
        if data["open"] != true {
            return o.yellow("The marketplace is closed right now.");
        }
        let templates = data["templates"].as_array().cloned().unwrap_or_default();
        if templates.is_empty() {
            return "Nothing here yet.".into();
        }
        table(o, &["ID", "COST", "INSTALLS", "MODE", "TITLE"], template_rows(&templates)) + &more(ctx, &data)
    });
    Ok(exit::OK)
}

fn market_search(ctx: &Ctx) -> Result<i32> {
    let q = ctx.args.join(" ");
    let data = ctx.api()?.call("GET", "marketplace", Call::new().q("q", Some(&q)))?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let templates = data["templates"].as_array().cloned().unwrap_or_default();
        if templates.is_empty() {
            return "No matches.".into();
        }
        table(o, &["ID", "COST", "INSTALLS", "MODE", "TITLE"], template_rows(&templates))
    });
    Ok(exit::OK)
}

fn market_show(ctx: &Ctx) -> Result<i32> {
    let data = ctx.api()?.get(&format!("marketplace/{}", seg(&ctx.args[0])))?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let t = &data["template"];
        let companions = num(&t["companions"]);
        let cost = format!(
            "{}{}",
            cats(num(&t["deployCostCats"])),
            if companions > 0.0 {
                format!(" (with {} helper agent{})", fmt_number(companions), if companions == 1.0 { "" } else { "s" })
            } else {
                String::new()
            }
        );
        [
            pairs(
                o,
                vec![
                    ("Title", Some(o.p.strong(Tone::Accent, &text(&t["title"])))),
                    ("Id", opt_text(&t["id"])),
                    ("Mode", opt_text(&t["mode"])),
                    ("Deploy cost", Some(o.yellow(&cost))),
                    ("Installs", opt_text(&t["installs"])),
                    ("Needs a login", if t["requiresLogin"] == true { Some("yes".into()) } else { None }),
                    ("Official", if t["official"] == true { Some(o.green("yes")) } else { None }),
                ],
            ),
            String::new(),
            text(&t["description"]),
            String::new(),
            format!("Deploy it: {}", o.accent(&format!("magine marketplace deploy {}", text(&t["id"])))),
        ]
        .join("\n")
    });
    Ok(exit::OK)
}

fn market_deploy(ctx: &Ctx) -> Result<i32> {
    let data = ctx.api()?.call("POST", &format!("marketplace/{}/deploy", seg(&ctx.args[0])), Call::new())?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let agents: Vec<String> = data["agents"].as_array().into_iter().flatten().map(text).collect();
        let failures: Vec<String> = data["companionFailures"].as_array().into_iter().flatten().map(text).collect();
        let mut lines = vec![format!("{}: {}. {} left.", o.ok("Deployed"), o.p.strong(Tone::Accent, &agents.join(", ")), cats(num(&data["catsRemaining"])))];
        if !failures.is_empty() {
            lines.push(o.yellow(&format!("Not added: {}", failures.join(", "))));
        }
        if let Some(first) = agents.first() {
            lines.push(format!("Run it: {}", o.accent(&format!("magine agents run {first}"))));
        }
        lines.join("\n")
    });
    Ok(exit::OK)
}

fn github(ctx: &Ctx) -> Result<i32> {
    let data = ctx.api()?.call("GET", &format!("github/{}", seg(&ctx.args[0])), Call::new().timeout_ms(180_000))?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let p = &data["profile"];
        let name = opt_text(&p["name"]).map(|n| format!(" ({n})")).unwrap_or_default();
        let languages: Vec<String> =
            p["languages"].as_array().into_iter().flatten().map(|l| format!("{} {}%", text(&l["name"]), num(&l["percentage"]).round())).collect();
        let tools: Vec<String> =
            data["ai"]["tools"].as_array().into_iter().flatten().map(|t| format!("{} {}%", text(&t["tool"]), text(&t["percentage"]))).collect();
        let code = if num(&p["linesOfCode"]) > 0.0 {
            Some(format!("{} lines, ~{} hours", format_tokens(num(&p["linesOfCode"])), format_tokens(num(&p["codingHours"]))))
        } else {
            None
        };
        let ai = if data["ai"].is_object() {
            Some(format!("{}% ({} confidence)", text(&data["ai"]["aiPercentage"]), text(&data["ai"]["confidence"])))
        } else {
            None
        };
        let worth = if num(&data["worth"]["estimatedUsdPerYear"]) > 0.0 {
            Some(format!("${}/year ({})", group_thousands(num(&data["worth"]["estimatedUsdPerYear"]).round()), text(&data["worth"]["tier"])))
        } else {
            None
        };
        pairs(
            o,
            vec![
                ("User", Some(format!("{}{name}", o.p.strong(Tone::Accent, &text(&data["user"]))))),
                ("Joined", opt_text(&p["joined"])),
                ("Followers", opt_text(&p["followers"])),
                ("Repos", opt_text(&p["publicRepos"])),
                ("Stars", opt_text(&p["totalStars"])),
                ("Commits", opt_text(&p["totalCommits"])),
                ("Streak", opt_text(&p["streakDays"]).map(|d| format!("{d} days"))),
                ("Code", code),
                ("Languages", Some(languages.join(", "))),
                ("AI-written", ai),
                ("AI tools", if tools.is_empty() { None } else { Some(tools.join(", ")) }),
                ("Est. worth", worth.map(|w| o.green(&w))),
            ],
        )
    });
    Ok(exit::OK)
}

pub fn commands() -> Vec<CommandSpec> {
    let ids = "Id from \"magine marketplace list\"";
    vec![
        CommandSpec::new("neurons", GROUP, "Search what your agents know (Neurons)", neurons)
            .aliases(&["neurons search"])
            .desc("Searches the Neurons map: your agents, the sites they work on, their memories and what Paws knows. Without words it lists the most important ones.")
            .arg("query", "Words to look for", false, true)
            .value("kind", KINDS_PLACEHOLDER, "Only this kind")
            .value("limit", "n", "How many (default 25, max 200)"),
        CommandSpec::new("marketplace list", GROUP, "Browse ready-made agents", market_list)
            .aliases(&["marketplace", "market"])
            .value("sort", "trending|date|cost", "Order (default trending)")
            .value("limit", "n", "How many (default 20, max 50)")
            .value("page", "n", "Page, from 0"),
        CommandSpec::new("marketplace search", GROUP, "Search ready-made agents by name", market_search).arg("words", "What to look for", true, true),
        CommandSpec::new("marketplace show", GROUP, "Show a ready-made agent", market_show).arg("id", ids, true, false),
        CommandSpec::new("marketplace deploy", GROUP, "Add a ready-made agent to your agents", market_deploy)
            .aliases(&["marketplace install"])
            .costs("1 Cat per agent it adds (shown by \"marketplace show\")")
            .arg("id", ids, true, false),
        CommandSpec::new("github", GROUP, "Analyse a GitHub profile", github).arg("username", "GitHub username", true, false),
    ]
}
