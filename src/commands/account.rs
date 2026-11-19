use crate::api::{seg, Call};
use crate::commands::{confirm_or_yes, obj, sv};
use crate::errors::{exit, usage_error, MagineError, Result};
use crate::open::{can_open_browser, open_browser};
use crate::output::{pairs, table, Cell, Tone};
use crate::registry::{CommandSpec, Ctx};
use crate::term;
use crate::util::{fmt_number, format_tokens, group_thousands, now_ms, num, opt_text, rel, text, truncate};
use serde_json::{json, Value};

fn confirm_payment(ctx: &Ctx, order: &str, provider: &str) -> Result<Value> {
    Ok(ctx.api()?.call("POST", &format!("wallet/checkout/{}/confirm", seg(order)), Call::new().json(json!({ "provider": provider })))?.data)
}

fn paid_text(ctx: &Ctx, d: &Value) -> String {
    let o = &ctx.out;
    match d["status"].as_str() {
        Some("completed") => {
            let granted = num(&d["tokensGranted"]);
            format!(
                "{} {}",
                o.ok("Paid."),
                if granted > 0.0 { format!("{} tokens added.", format_tokens(granted)) } else { "Your Cats are in your wallet.".into() }
            )
        }
        Some("failed") => o.p.strong(Tone::Red, "The payment did not go through."),
        _ => format!("{} {}", o.yellow("Not paid yet."), text(&d["message"])).trim().to_string(),
    }
}

fn wallet(ctx: &Ctx) -> Result<i32> {
    let data = ctx.api()?.get("wallet")?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let b = &data["balance"];
        let mut parts = vec![pairs(
            o,
            vec![
                (
                    "Tokens",
                    Some(format!(
                        "{} ({} free, {} paid)",
                        o.p.strong(Tone::Accent, &group_thousands(num(&b["tokens"]))),
                        format_tokens(num(&b["freeTokens"])),
                        format_tokens(num(&b["paidTokens"]))
                    )),
                ),
                ("Cats", Some(format!("{} (each makes one agent)", o.bold(&text(&b["cats"]))))),
                ("Agent slots", opt_text(&b["agentSlots"])),
                (
                    "Price",
                    Some(format!("1 Cat = ${} = {} tokens", text(&data["pricing"]["catPriceUsd"]), format_tokens(num(&data["pricing"]["tokensPerCat"])))),
                ),
            ],
        )];
        let packages = data["packages"].as_array().cloned().unwrap_or_default();
        if !packages.is_empty() {
            parts.push(String::new());
            parts.push(table(
                o,
                &["CATS", "PRICE", "TOKENS"],
                packages
                    .iter()
                    .map(|p| {
                        vec![
                            Cell::tone(text(&p["cats"]), Tone::Accent),
                            Cell::tone(format!("${}", text(&p["priceUsd"])), Tone::Green),
                            Cell::from(format_tokens(num(&p["tokens"]))),
                        ]
                    })
                    .collect(),
            ));
        }
        if data["providers"].as_array().is_some_and(|p| !p.is_empty()) {
            parts.push(String::new());
            parts.push(format!("Buy with: {}", o.accent("magine wallet buy <cats>")));
        }
        parts.join("\n")
    });
    Ok(exit::OK)
}

fn usage(ctx: &Ctx) -> Result<i32> {
    let data = ctx.api()?.call("GET", "wallet/usage", Call::new().q("limit", ctx.opt("limit")).q("page", ctx.opt("page")))?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let usage = data["usage"].as_array().cloned().unwrap_or_default();
        if usage.is_empty() {
            return "No usage yet.".into();
        }
        table(
            o,
            &["WHEN", "TOKENS", "ACTION", "DETAILS"],
            usage
                .iter()
                .map(|u| {
                    vec![
                        Cell::from(rel(&u["at"])),
                        Cell::tone(format_tokens(num(&u["tokensUsed"])), Tone::Yellow),
                        Cell::from(text(&u["action"])),
                        Cell::from(truncate(&text(&u["details"]), 200)),
                    ]
                })
                .collect(),
        )
    });
    Ok(exit::OK)
}

fn buy(ctx: &Ctx) -> Result<i32> {
    let cats: u32 =
        ctx.args[0].trim().parse().ok().filter(|n| (1..=100).contains(n)).ok_or_else(|| usage_error("Cats must be a whole number from 1 to 100."))?;
    let data =
        ctx.api()?.call("POST", "wallet/checkout", Call::new().json(obj(vec![("cats", Some(json!(cats))), ("provider", sv(ctx.opt("provider")))])))?.data;
    let order = text(&data["orderId"]);
    let provider = text(&data["provider"]);
    let url = text(&data["url"]);
    let confirm_cmd = format!("magine wallet confirm {order} --provider {provider}");
    let opened = ctx.opts.not_off("open") && can_open_browser(&ctx.env, std::env::consts::OS) && open_browser(&url);
    let o = &ctx.out;
    let s = if cats == 1 { "" } else { "s" };
    if !ctx.interactive || ctx.opts.bool("wait") == Some(false) {
        let mut out = json!({ "step": "pay" });
        if let (Some(m), Some(d)) = (out.as_object_mut(), data.as_object()) {
            for (k, v) in d {
                m.insert(k.clone(), v.clone());
            }
            m.insert("browserOpened".into(), json!(opened));
            m.insert(
                "message".into(),
                json!(format!("The user pays ${} on the checkout page. Afterwards run: {confirm_cmd} --json", text(&data["amountUsd"]))),
            );
            m.insert("next".into(), json!(format!("{confirm_cmd} --json")));
        }
        o.result(&out, || {
            format!("Pay {} for {cats} Cat{s} at:\n{}\nThen run: {}", o.bold(&format!("${}", text(&data["amountUsd"]))), o.link(&url), o.accent(&confirm_cmd))
        });
        return Ok(exit::OK);
    }
    o.line(&format!("Pay {} for {cats} Cat{s} ({} tokens):", o.bold(&format!("${}", text(&data["amountUsd"]))), format_tokens(num(&data["tokens"]))));
    o.line(&o.link(&url));
    o.note(&format!(
        "{}Waiting for the payment... (Ctrl+C stops waiting; check later with \"{confirm_cmd}\")",
        if opened { "Opened in your browser. " } else { "" }
    ));
    let deadline = now_ms() + 20 * 60_000;
    let _waiting = term::Waiting::start();
    while now_ms() < deadline {
        if let Err(e) = term::sleep_ms(5000) {
            return Err(e.hint(format!("When you have paid, run: {confirm_cmd}")));
        }
        let d = match confirm_payment(ctx, &order, &provider) {
            Ok(d) => d,
            Err(e) if e.exit_code == exit::NETWORK => json!({ "status": "pending" }),
            Err(e) => return Err(e),
        };
        if d["status"] == "completed" || d["status"] == "failed" {
            o.result(&d, || paid_text(ctx, &d));
            return Ok(if d["status"] == "completed" { exit::OK } else { exit::FAILED });
        }
    }
    Err(MagineError::new("No payment yet.", exit::WAITING, "waiting").hint(format!("When you have paid, run: {confirm_cmd}")))
}

fn confirm(ctx: &Ctx) -> Result<i32> {
    let provider = ctx.opt("provider").unwrap_or("");
    if provider != "dodo" && provider != "paypal" {
        return Err(usage_error("Say which payment method the order used: --provider dodo or --provider paypal."));
    }
    let d = confirm_payment(ctx, &ctx.args[0], provider)?;
    ctx.out.result(&d, || paid_text(ctx, &d));
    Ok(match d["status"].as_str() {
        Some("completed") => exit::OK,
        Some("failed") => exit::FAILED,
        _ => exit::WAITING,
    })
}

fn keys_list(ctx: &Ctx) -> Result<i32> {
    let data = ctx.api()?.get("keys")?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let keys = data["keys"].as_array().cloned().unwrap_or_default();
        if keys.is_empty() {
            return "No API keys.".into();
        }
        table(
            o,
            &["ID", "SOURCE", "LAST USED", "EXPIRES", "NAME"],
            keys.iter()
                .map(|k| {
                    let current = k["current"] == true;
                    vec![
                        Cell::tone(text(&k["id"]), Tone::Muted),
                        Cell::from(text(&k["source"])),
                        Cell::from(rel(&k["lastUsedAt"])),
                        Cell::from(if k["expiresAt"].is_null() { "never".to_string() } else { rel(&k["expiresAt"]) }),
                        if current { Cell::tone(format!("{} (this one)", text(&k["name"])), Tone::Green) } else { Cell::from(text(&k["name"])) },
                    ]
                })
                .collect(),
        )
    });
    Ok(exit::OK)
}

fn keys_create(ctx: &Ctx) -> Result<i32> {
    let days = match ctx.opt("expires-in-days") {
        Some(d) => Some(
            d.trim()
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=365).contains(n))
                .ok_or_else(|| usage_error("--expires-in-days must be a whole number from 1 to 365."))?,
        ),
        None => None,
    };
    let data = ctx.api()?.call("POST", "keys", Call::new().json(obj(vec![("name", sv(ctx.opt("name"))), ("expiresInDays", days.map(|d| json!(d)))])))?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let expires = opt_text(&data["expiresAt"]).map(|e| format!(" (expires {})", &e[..e.len().min(10)])).unwrap_or_default();
        [
            format!("{} {}{expires}:", o.ok("Created"), text(&data["prefix"])),
            String::new(),
            o.p.strong(Tone::Accent, &text(&data["key"])),
            String::new(),
            o.yellow("Shown only now. Keep it in a secret store or an environment variable (MAGINE_API_KEY); anyone with it can use your account."),
        ]
        .join("\n")
    });
    Ok(exit::OK)
}

fn keys_revoke(ctx: &Ctx) -> Result<i32> {
    confirm_or_yes(ctx, &format!("Revoke the API key {}? Anything using it stops working.", ctx.args[0]))?;
    let data = ctx.api()?.call("DELETE", &format!("keys/{}", seg(&ctx.args[0])), Call::new())?.data;
    let o = &ctx.out;
    o.result(&data, || {
        let n = num(&data["revoked"]);
        let many = if n > 1.0 { format!(" {} keys", fmt_number(n)) } else { String::new() };
        let current = if data["current"] == true { format!(" That was the key this CLI uses: run {}.", o.accent("\"magine login\"")) } else { String::new() };
        format!("{}{many}.{current}", o.ok("Revoked"))
    });
    Ok(exit::OK)
}

pub fn wallet_commands() -> Vec<CommandSpec> {
    let provider = "Payment method of the order (shown by \"wallet buy\")";
    vec![
        CommandSpec::new("wallet", "Wallet", "Show your tokens, Cats and prices", wallet).aliases(&["wallet balance", "balance"]),
        CommandSpec::new("wallet usage", "Wallet", "Show what used your tokens", usage).value("limit", "n", "How many (default 20, max 100)").value("page", "n", "Page, from 0"),
        CommandSpec::new("wallet buy", "Wallet", "Buy Cats: opens a checkout page for you to pay", buy)
            .aliases(&["buy"])
            .desc("Creates a checkout and opens it in your browser. The CLI never pays by itself: you pay on the checkout page. In a terminal it waits and confirms the payment; otherwise run \"magine wallet confirm\" afterwards.")
            .arg("cats", "How many Cats (1-100); \"magine wallet\" lists the packages", true, false)
            .value("provider", "dodo|paypal", "Payment method (default: what is available)")
            .flag("open", "Open the checkout in the browser (--no-open only prints the link)")
            .flag("wait", "Wait for the payment (default in a terminal)"),
        CommandSpec::new("wallet confirm", "Wallet", "Check a payment and add its Cats to your wallet", confirm)
            .arg("order", "Order id from \"magine wallet buy\"", true, false)
            .value("provider", "dodo|paypal", provider),
    ]
}

pub fn key_commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("keys list", "API keys", "List your API keys and signed-in computers", keys_list).aliases(&["keys", "keys ls"]),
        CommandSpec::new("keys create", "API keys", "Create an API key (shown once)", keys_create)
            .aliases(&["keys new"])
            .desc("Creates a key for a script or server. It stops working when the key that made it is revoked, and never outlives it.")
            .value("name", "name", "What it is for")
            .value("expires-in-days", "days", "Expire after this many days (1-365)"),
        CommandSpec::new("keys revoke", "API keys", "Revoke an API key (and keys it made)", keys_revoke)
            .aliases(&["keys rm"])
            .arg("id", "Key id from \"magine keys list\"", true, false)
            .flag_alias("yes", "y", "Do not ask"),
    ]
}
