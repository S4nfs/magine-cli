//! The Magine look: the cat, the wordmark and web-terminal-style sections, in plain ASCII.

use crate::output::{Painter, Tone};

const CAT: [&str; 3] = [r" /\_/\ ", r"( o.o )", r" > ^ < "];

const WORDMARK: [&str; 5] = [
    r" __  __    _    ____ ___ _   _ _____ ",
    r"|  \/  |  / \  / ___|_ _| \ | | ____|",
    r"| |\/| | / _ \| |  _ | ||  \| |  _|  ",
    r"| |  | |/ ___ \ |_| || || |\  | |___ ",
    r"|_|  |_/_/   \_\____|___|_| \_|_____|",
];

/// The cat beside the MAGINE wordmark, then a tagline.
pub fn banner(p: &Painter, version: &str, tagline: &str) -> String {
    let mut lines = Vec::with_capacity(7);
    for (i, row) in WORDMARK.iter().enumerate() {
        let cat = if (1..=3).contains(&i) { CAT[i - 1] } else { "       " };
        let line = format!("  {}  {}", p.strong(Tone::Cat, cat), p.strong(Tone::Accent, row.trim_end()));
        lines.push(line.trim_end().to_string());
    }
    lines.push(format!("  {} {} {} {}", p.strong(Tone::Accent, "Magine CLI"), p.tone(Tone::Green, &format!("v{version}")), p.muted("-"), p.muted(tagline)));
    lines.join("\n")
}

/// A one-line mark for short screens: `=^.^= Magine`.
pub fn mark(p: &Painter, title: &str) -> String {
    format!("{} {}", p.strong(Tone::Cat, "=^.^="), p.strong(Tone::Accent, title))
}

/// A section title underlined with 60 dashes, as in the Magine terminal's help.
pub fn section(p: &Painter, title: &str) -> String {
    format!("{}\n{}", p.strong(Tone::Accent, &title.to_uppercase()), p.muted(&"-".repeat(60)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_brand_is_plain_ascii_without_colours() {
        let p = Painter::default();
        let b = banner(&p, "1.7.5", "your agents");
        assert!(b.is_ascii());
        assert!(b.contains("( o.o )"));
        assert!(b.contains("Magine CLI v1.7.5"));
        assert_eq!(section(&p, "Agents"), format!("AGENTS\n{}", "-".repeat(60)));
        assert!(!b.contains('\x1b'));
    }

    #[test]
    fn colours_wrap_the_text() {
        let p = Painter { on: true, truecolor: false };
        assert_eq!(p.tone(Tone::Green, "ok"), "\x1b[32mok\x1b[39m");
        let t = Painter { on: true, truecolor: true };
        assert!(t.tone(Tone::Accent, "x").starts_with("\x1b[38;2;88;166;255m"));
    }
}
