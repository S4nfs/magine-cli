//! `@agent` and `#file` tags in task text, read the way the Magine server reads them
//! (resolveAgentReferences and resolveFileReferences in lib/catbot.ts, lib/file-tag.ts).

use crate::api::{Api, Call};
use crate::errors::Result;
use crate::registry::Ctx;
use crate::util::{format_bytes, num, opt_text, rel, text};

/// Cattery pages read for suggestions (60 files each, newest first).
const FILE_PAGES: u64 = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Agent,
    File,
}

impl Kind {
    pub fn sigil(self) -> char {
        match self {
            Kind::Agent => '@',
            Kind::File => '#',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Agent => "agent",
            Kind::File => "file",
        }
    }
}

/// A tag written in a task: `@news-bot` is Kind::Agent named "news-bot".
#[derive(Clone, PartialEq, Debug)]
pub struct Found {
    pub kind: Kind,
    pub name: String,
}

/// One of your agents or Cattery files, as it can be tagged.
#[derive(Clone, Debug)]
pub struct Taggable {
    pub kind: Kind,
    /// What goes after the sigil.
    pub tag: String,
    /// The agent's or file's own name.
    pub name: String,
    pub id: String,
    pub detail: String,
}

impl Taggable {
    pub fn full(&self) -> String {
        format!("{}{}", self.kind.sigil(), self.tag)
    }
}

pub fn is_name_char(c: char, kind: Kind) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' || (kind == Kind::File && c == '/')
}

/// The server reads a tag only at the start or after a space or one of ( [ { , ; >
pub fn is_boundary(prev: Option<char>) -> bool {
    prev.is_none_or(|c| c.is_whitespace() || "([{,;>".contains(c))
}

/// The tags in `text`, as the server finds them.
pub fn find(text: &str) -> Vec<Found> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Found> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let kind = match chars[i] {
            '@' => Kind::Agent,
            '#' => Kind::File,
            _ => {
                i += 1;
                continue;
            }
        };
        if !is_boundary(i.checked_sub(1).map(|p| chars[p])) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && is_name_char(chars[j], kind) {
            j += 1;
        }
        let mut name: String = chars[i + 1..j].iter().collect();
        match kind {
            // `@name:` is no tag: the server keeps `@bot:<id>` out.
            Kind::Agent if chars.get(j) == Some(&':') => name.clear(),
            // A file tag ends on a letter, digit or `_`.
            Kind::File => name = name.trim_end_matches(['.', '/', '-']).to_string(),
            Kind::Agent => {}
        }
        if !name.is_empty() && !out.iter().any(|f| f.kind == kind && f.name == name) {
            out.push(Found { kind, name });
        }
        i = j.max(i + 1);
    }
    out
}

/// A Cattery file name as its tag: characters a tag cannot hold become `_` (lib/file-tag.ts).
pub fn file_tag_name(name: &str) -> String {
    let mut s = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
            s.push(c);
        } else {
            // JavaScript replaces each UTF-16 unit.
            s.extend(std::iter::repeat_n('_', c.len_utf16()));
        }
    }
    s
}

/// Whether `name` (after the sigil) picks `t`, the way the server looks it up.
pub fn matches(t: &Taggable, kind: Kind, name: &str) -> bool {
    if t.kind != kind {
        return false;
    }
    let (a, b) = (t.tag.to_ascii_lowercase(), name.to_ascii_lowercase());
    match kind {
        Kind::Agent => a == b,
        Kind::File => a == b || (!b.contains('.') && a.starts_with(&format!("{b}."))),
    }
}

fn in_order(query: &str, name: &str) -> bool {
    let mut rest = name.chars();
    query.chars().all(|q| rest.any(|c| c == q))
}

/// The best matches for `query`: names that start with it, then names that contain it,
/// then names that hold its letters in order. Ties keep the list order (files: newest first).
pub fn suggest<'a>(items: &'a [Taggable], kind: Kind, query: &str, limit: usize) -> Vec<&'a Taggable> {
    let q = query.to_ascii_lowercase();
    let mut ranked: Vec<(u8, usize, &Taggable)> = items
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == kind)
        .filter_map(|(i, t)| {
            let n = t.tag.to_ascii_lowercase();
            let rank = if n.starts_with(&q) {
                0
            } else if n.contains(&q) {
                1
            } else if in_order(&q, &n) {
                2
            } else {
                return None;
            };
            Some((rank, i, t))
        })
        .collect();
    ranked.sort_by_key(|(rank, i, _)| (*rank, *i));
    ranked.into_iter().take(limit).map(|(_, _, t)| t).collect()
}

fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The taggable a mistyped `name` most likely meant.
pub fn closest<'a>(items: &'a [Taggable], kind: Kind, name: &str) -> Option<&'a Taggable> {
    let trimmed = name.trim_end_matches(['.', '-', '_']);
    if let Some(t) = items.iter().find(|t| matches(t, kind, trimmed)) {
        return Some(t);
    }
    let lower = trimmed.to_ascii_lowercase();
    items
        .iter()
        .filter(|t| t.kind == kind)
        .map(|t| (distance(&lower, &t.tag.to_ascii_lowercase()), t))
        .filter(|(d, t)| *d <= 2.max(t.tag.len() / 4))
        .min_by_key(|(d, _)| *d)
        .map(|(_, t)| t)
        .or_else(|| suggest(items, kind, trimmed, 1).into_iter().next())
}

fn agent_detail(a: &serde_json::Value) -> String {
    let mut parts = vec![if a["system"] == "paws" { "Paws, in your browser".to_string() } else { text(&a["mode"]) }];
    parts.push(if a["active"] == true { "on".into() } else { "off".into() });
    if let Some(s) = opt_text(&a["schedule"]) {
        parts.push(s);
    }
    parts.join(", ")
}

/// Your agents (Paws ones too) and newest Cattery files. The flag says whether the file list is whole.
pub fn load(api: &Api, agents: bool, files: bool) -> Result<(Vec<Taggable>, bool)> {
    let mut out = Vec::new();
    let mut complete = true;
    if agents {
        let data = api.call("GET", "agents", Call::new().q("all", Some("1")))?.data;
        for a in data["agents"].as_array().into_iter().flatten() {
            let name = text(&a["name"]);
            if !name.is_empty() {
                out.push(Taggable { kind: Kind::Agent, tag: name.clone(), name, id: text(&a["id"]), detail: agent_detail(a) });
            }
        }
    }
    if files {
        let mut page = 0u64;
        loop {
            let p = page.to_string();
            let data = api.call("GET", "cattery", Call::new().q("limit", Some("60")).q("page", Some(&p)))?.data;
            for i in data["items"].as_array().into_iter().flatten() {
                let name = text(&i["name"]);
                let tag = file_tag_name(&name);
                if tag.is_empty() || out.iter().any(|t: &Taggable| t.kind == Kind::File && t.tag.eq_ignore_ascii_case(&tag)) {
                    continue;
                }
                let detail = format!("{}, {}, {}", text(&i["kind"]), format_bytes(num(&i["bytes"])), rel(&i["createdAt"]));
                out.push(Taggable { kind: Kind::File, tag, name, id: text(&i["id"]), detail });
            }
            match data["nextPage"].as_u64() {
                Some(next) if next > page && next < FILE_PAGES => page = next,
                Some(_) => {
                    complete = false;
                    break;
                }
                None => break,
            }
        }
    }
    Ok((out, complete))
}

/// Warns about tags that name none of your agents or files; a check that fails to load stays quiet.
pub fn check(ctx: &Ctx, text: &str, known: Option<(&[Taggable], bool)>) {
    let found = find(text);
    let agents = found.iter().any(|f| f.kind == Kind::Agent);
    let files = found.iter().any(|f| f.kind == Kind::File && !f.name.contains('/'));
    if !agents && !files {
        return;
    }
    let loaded;
    let (items, complete) = match known {
        Some(k) => k,
        None => {
            let Ok(api) = ctx.api() else { return };
            let Ok(l) = load(&api, agents, files) else { return };
            loaded = l;
            (&loaded.0[..], loaded.1)
        }
    };
    for f in &found {
        if items.iter().any(|t| matches(t, f.kind, &f.name)) || (f.kind == Kind::File && (f.name.contains('/') || !complete)) {
            continue;
        }
        let mut msg = match f.kind {
            Kind::Agent => format!("@{} is not one of your agents, so it goes as plain text (like a social handle).", f.name),
            Kind::File => format!("#{} is not in your Cattery, so the agent gets no file for it.", f.name),
        };
        if let Some(t) = closest(items, f.kind, &f.name) {
            msg.push_str(&format!(" Did you mean {}?", t.full()));
        }
        ctx.out.warn(&msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: Kind, tag: &str) -> Taggable {
        Taggable { kind, tag: tag.into(), name: tag.into(), id: format!("id-{tag}"), detail: String::new() }
    }

    fn names(text: &str) -> Vec<String> {
        find(text).into_iter().map(|f| format!("{}{}", f.kind.sigil(), f.name)).collect()
    }

    #[test]
    fn tags_are_found_where_the_server_finds_them() {
        assert_eq!(names("Summarise #report.pdf for @news-bot"), ["#report.pdf", "@news-bot"]);
        assert_eq!(names("(@a, #b.md) [#c] {@d} ;#e >@f"), ["@a", "#b.md", "#c", "@d", "#e", "@f"]);
        assert_eq!(names("mail bob@example.com or see https://youtube.com/@chan and page#top"), Vec::<String>::new());
        assert_eq!(names("ask @bot:5f0e and @x: now"), Vec::<String>::new());
        assert_eq!(names("read #notes. then #dir/file.md/ and #a-"), ["#notes", "#dir/file.md", "#a"]);
        assert_eq!(names("@news-bot. and @news-bot"), ["@news-bot.", "@news-bot"]);
        assert_eq!(names("#  @"), Vec::<String>::new());
        assert_eq!(names("@x @x #y #y"), ["@x", "#y"]);
    }

    #[test]
    fn file_names_become_tags_like_on_the_platform() {
        assert_eq!(file_tag_name("my resume (final).pdf"), "my_resume__final_.pdf");
        assert_eq!(file_tag_name("report-2026_v2.md"), "report-2026_v2.md");
        assert_eq!(file_tag_name("résumé.pdf"), "r_sum_.pdf");
        assert_eq!(file_tag_name("cat\u{1F431}.png"), "cat__.png");
    }

    #[test]
    fn tags_match_names_and_file_base_names() {
        let a = item(Kind::Agent, "News-Bot");
        assert!(matches(&a, Kind::Agent, "news-bot"));
        assert!(!matches(&a, Kind::File, "news-bot"));
        let f = item(Kind::File, "report.pdf");
        assert!(matches(&f, Kind::File, "REPORT.pdf"));
        assert!(matches(&f, Kind::File, "report"));
        assert!(!matches(&f, Kind::File, "report.md"));
        assert!(!matches(&f, Kind::File, "rep"));
    }

    #[test]
    fn suggestions_rank_prefix_then_contains_then_letters_in_order() {
        let items = vec![item(Kind::Agent, "jobs-digest"), item(Kind::Agent, "news-bot"), item(Kind::Agent, "hn-news"), item(Kind::File, "news.md")];
        let tags: Vec<&str> = suggest(&items, Kind::Agent, "news", 8).iter().map(|t| t.tag.as_str()).collect();
        assert_eq!(tags, ["news-bot", "hn-news"]);
        let tags: Vec<&str> = suggest(&items, Kind::Agent, "jd", 8).iter().map(|t| t.tag.as_str()).collect();
        assert_eq!(tags, ["jobs-digest"]);
        assert_eq!(suggest(&items, Kind::Agent, "", 2).len(), 2);
        assert!(suggest(&items, Kind::Agent, "zzz", 8).is_empty());
    }

    #[test]
    fn mistyped_tags_find_what_they_meant() {
        let items = vec![item(Kind::Agent, "news-bot"), item(Kind::Agent, "price-watch"), item(Kind::File, "report.pdf")];
        assert_eq!(closest(&items, Kind::Agent, "news-bot.").map(|t| t.full()), Some("@news-bot".into()));
        assert_eq!(closest(&items, Kind::Agent, "newsbot").map(|t| t.full()), Some("@news-bot".into()));
        assert_eq!(closest(&items, Kind::Agent, "price").map(|t| t.full()), Some("@price-watch".into()));
        assert_eq!(closest(&items, Kind::File, "reprot.pdf").map(|t| t.full()), Some("#report.pdf".into()));
        assert!(closest(&items, Kind::Agent, "elonmusk").is_none());
    }
}
