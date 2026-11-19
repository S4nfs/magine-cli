//! A one-line task editor for the terminal. Typing `@` or `#` opens a dropdown of your
//! agents or Cattery files, like the @ and # menus in the Magine web terminal.

use crate::api::Api;
use crate::errors::{exit, MagineError, Result};
use crate::output::{Painter, Tone};
use crate::tags::{self, Kind, Taggable};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use std::io::Write;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

const ROWS: usize = 6;

/// What the editor knows about your agents and files, once they have loaded.
pub type Known = (Vec<Taggable>, bool);

#[derive(Debug, PartialEq)]
enum Step {
    Continue,
    Submit,
    Cancel,
}

/// The text being edited and the dropdown state, kept apart from the terminal so it can be tested.
struct State {
    buf: Vec<char>,
    cur: usize,
    known: Option<Known>,
    sel: usize,
    /// The sigil position whose dropdown Esc closed.
    dismissed: Option<usize>,
    last_query: Option<(usize, String)>,
}

impl State {
    fn new() -> Self {
        State { buf: Vec::new(), cur: 0, known: None, sel: 0, dismissed: None, last_query: None }
    }

    fn text(&self) -> String {
        self.buf.iter().collect()
    }

    /// The tag being typed at the cursor: its kind, where its sigil is, and what follows it so far.
    fn token(&self) -> Option<(Kind, usize, String)> {
        let mut start = self.cur;
        while start > 0 && tags::is_name_char(self.buf[start - 1], Kind::File) {
            start -= 1;
        }
        let at = start.checked_sub(1)?;
        let kind = match self.buf[at] {
            '@' => Kind::Agent,
            '#' => Kind::File,
            _ => return None,
        };
        if !tags::is_boundary(at.checked_sub(1).map(|p| self.buf[p])) {
            return None;
        }
        let query: String = self.buf[start..self.cur].iter().collect();
        if kind == Kind::Agent && query.contains('/') {
            return None;
        }
        Some((kind, at, query))
    }

    /// The open dropdown: the sigil position and the suggestions (empty while loading or when nothing matches).
    fn dropdown(&mut self) -> Option<(Kind, usize, String, Vec<Taggable>)> {
        let (kind, at, query) = self.token()?;
        if self.dismissed == Some(at) {
            return None;
        }
        if self.last_query.as_ref() != Some(&(at, query.clone())) {
            self.sel = 0;
            self.last_query = Some((at, query.clone()));
        }
        let list: Vec<Taggable> = match &self.known {
            Some((items, _)) => tags::suggest(items, kind, &query, ROWS).into_iter().cloned().collect(),
            None => Vec::new(),
        };
        if !list.is_empty() {
            self.sel = self.sel.min(list.len() - 1);
        }
        Some((kind, at, query, list))
    }

    fn insert(&mut self, c: char) {
        self.buf.insert(self.cur, c);
        self.cur += 1;
        self.dismissed = None;
    }

    /// Puts `t` in place of the tag being typed, followed by a space.
    fn accept(&mut self, at: usize, t: &Taggable) {
        let mut end = self.cur;
        while end < self.buf.len() && tags::is_name_char(self.buf[end], t.kind) {
            end += 1;
        }
        let tag: Vec<char> = t.full().chars().collect();
        let len = tag.len();
        self.buf.splice(at..end, tag);
        self.cur = at + len;
        if self.buf.get(self.cur).is_none_or(|c| !c.is_whitespace()) {
            self.buf.insert(self.cur, ' ');
        }
        self.cur += 1;
    }

    fn delete_word(&mut self) {
        let mut start = self.cur;
        while start > 0 && self.buf[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !self.buf[start - 1].is_whitespace() {
            start -= 1;
        }
        self.buf.drain(start..self.cur);
        self.cur = start;
    }

    /// `more` is true when further keys are already waiting: an Enter among them is part of a paste.
    fn key(&mut self, k: KeyEvent, more: bool) -> Step {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let open = self.dropdown();
        let chosen = open.as_ref().and_then(|(_, at, _, list)| list.get(self.sel).map(|t| (*at, t.clone())));
        match k.code {
            KeyCode::Char('c') if ctrl && !alt => return Step::Cancel,
            KeyCode::Char('d') if ctrl && !alt => {
                if self.buf.is_empty() {
                    return Step::Cancel;
                }
                if self.cur < self.buf.len() {
                    self.buf.remove(self.cur);
                }
            }
            KeyCode::Char('a') if ctrl && !alt => self.cur = 0,
            KeyCode::Char('e') if ctrl && !alt => self.cur = self.buf.len(),
            KeyCode::Char('u') if ctrl && !alt => {
                self.buf.drain(..self.cur);
                self.cur = 0;
            }
            KeyCode::Char('w') if ctrl && !alt => self.delete_word(),
            // AltGr arrives as Ctrl+Alt on Windows, and many keyboards type @ and # with it.
            KeyCode::Char(c) if !ctrl || alt => self.insert(c),
            KeyCode::Backspace if self.cur > 0 => {
                self.cur -= 1;
                self.buf.remove(self.cur);
                self.dismissed = None;
            }
            KeyCode::Delete if self.cur < self.buf.len() => {
                self.buf.remove(self.cur);
            }
            KeyCode::Left => self.cur = self.cur.saturating_sub(1),
            KeyCode::Right => self.cur = (self.cur + 1).min(self.buf.len()),
            KeyCode::Home => self.cur = 0,
            KeyCode::End => self.cur = self.buf.len(),
            KeyCode::Up => {
                if let Some((_, _, _, list)) = &open {
                    if !list.is_empty() {
                        self.sel = (self.sel + list.len() - 1) % list.len();
                    }
                }
            }
            KeyCode::Down => {
                if let Some((_, _, _, list)) = &open {
                    if !list.is_empty() {
                        self.sel = (self.sel + 1) % list.len();
                    }
                }
            }
            KeyCode::Tab => {
                if let Some((at, t)) = chosen {
                    self.accept(at, &t);
                }
            }
            KeyCode::Enter if more => self.insert(' '),
            KeyCode::Enter => {
                if let Some((at, t)) = chosen {
                    self.accept(at, &t);
                } else if self.buf.iter().any(|c| !c.is_whitespace()) {
                    return Step::Submit;
                }
            }
            KeyCode::Esc => {
                if let Some((_, at, _, _)) = open {
                    self.dismissed = Some(at);
                }
            }
            _ => {}
        }
        Step::Continue
    }

    fn handle(&mut self, ev: Event, more: bool) -> Step {
        match ev {
            Event::Paste(s) => {
                for c in s.chars() {
                    self.insert(if c.is_control() { ' ' } else { c });
                }
                Step::Continue
            }
            Event::Key(k) if k.kind != KeyEventKind::Release => self.key(k, more),
            _ => Step::Continue,
        }
    }
}

/// Terminal columns a character takes: two for wide East Asian letters and most emoji.
fn width(c: char) -> usize {
    let u = c as u32;
    let wide = matches!(u, 0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1F64F | 0x1F900..=0x1F9FF | 0x20000..=0x3FFFD);
    if wide {
        2
    } else {
        1
    }
}

fn width_of(s: impl IntoIterator<Item = char>) -> usize {
    s.into_iter().map(width).sum()
}

/// `s` cut to `max` columns.
fn fit(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        if w + width(c) > max {
            break;
        }
        w += width(c);
        out.push(c);
    }
    out
}

struct Screen<'a> {
    p: &'a Painter,
    label: String,
    hint: String,
    /// Rows between the editor's first row and the cursor, as last drawn.
    above: usize,
}

impl Screen<'_> {
    fn below(&self, st: &mut State, cols: usize) -> Vec<String> {
        let p = self.p;
        let Some((kind, _, query, list)) = st.dropdown() else {
            return vec![p.muted(&fit(&self.hint, cols - 1))];
        };
        if st.known.is_none() {
            return vec![p.muted(&fit("Loading your agents and files...", cols - 1))];
        }
        if list.is_empty() {
            let what = if kind == Kind::Agent { "agent" } else { "Cattery file" };
            return vec![p.muted(&fit(&format!("No {what} matches {}{query}. Esc closes this.", kind.sigil()), cols - 1))];
        }
        let tag_w = list.iter().map(|t| width_of(t.full().chars())).max().unwrap_or(0).min(36);
        let mut lines: Vec<String> = list
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let tag = fit(&t.full(), tag_w);
                let pad = " ".repeat(tag_w - width_of(tag.chars()));
                let room = (cols - 1).saturating_sub(tag_w + 4);
                let detail = fit(&t.detail, room);
                if i == st.sel {
                    format!("{} {}{pad}  {}", p.strong(Tone::Accent, ">"), p.strong(Tone::Accent, &tag), p.muted(&detail))
                } else {
                    format!("  {tag}{pad}  {}", p.muted(&detail))
                }
            })
            .collect();
        lines.push(p.muted(&fit("Up/Down choose, Tab or Enter inserts, Esc closes", cols - 1)));
        lines
    }

    fn draw(&mut self, st: &mut State, out: &mut impl Write, done: bool) -> std::io::Result<()> {
        let cols = terminal::size().map(|(c, _)| c as usize).unwrap_or(80).max(20);
        write!(out, "\r")?;
        if self.above > 0 {
            write!(out, "\x1b[{}A", self.above)?;
        }
        write!(out, "\x1b[J{} {}", self.p.strong(Tone::Accent, &self.label), st.text())?;
        let label_w = width_of(self.label.chars()) + 1;
        let total = label_w + width_of(st.buf.iter().copied());
        if total.is_multiple_of(cols) {
            // Leave the terminal's pending-wrap state so the next row exists.
            write!(out, "\r\n")?;
        }
        let rows = total / cols + 1;
        let mut extra = 0;
        if !done {
            for line in self.below(st, cols) {
                write!(out, "\r\n{line}")?;
                extra += 1;
            }
        }
        let pos = label_w + width_of(st.buf[..st.cur].iter().copied());
        let (row, col) = (pos / cols, pos % cols);
        let up = rows - 1 + extra - row;
        write!(out, "\r")?;
        if up > 0 {
            write!(out, "\x1b[{up}A")?;
        }
        if col > 0 {
            write!(out, "\x1b[{col}C")?;
        }
        self.above = row;
        out.flush()
    }
}

struct RawMode;

impl RawMode {
    fn on() -> std::io::Result<RawMode> {
        terminal::enable_raw_mode()?;
        #[cfg(not(windows))]
        let _ = crossterm::execute!(std::io::stderr(), event::EnableBracketedPaste);
        Ok(RawMode)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        #[cfg(not(windows))]
        let _ = crossterm::execute!(std::io::stderr(), event::DisableBracketedPaste);
        let _ = terminal::disable_raw_mode();
    }
}

/// Loads your agents and files in the background, so the editor opens at once.
pub fn load_in_background(api: Api) -> Receiver<Known> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(tags::load(&api, true, true).unwrap_or((Vec::new(), false)));
    });
    rx
}

fn io_err(e: std::io::Error) -> MagineError {
    MagineError::failed(format!("The terminal could not be read: {e}"))
}

fn is_release(ev: &Event) -> bool {
    matches!(ev, Event::Key(k) if k.kind == KeyEventKind::Release)
}

/// Whether more input is already waiting (keys of a paste); key releases, which Windows reports, do not count.
fn more_waiting(pending: &mut Option<Event>) -> std::io::Result<bool> {
    while event::poll(Duration::ZERO)? {
        let ev = event::read()?;
        if !is_release(&ev) {
            *pending = Some(ev);
            return Ok(true);
        }
    }
    Ok(false)
}

/// Reads one task in the terminal. Returns the text and what was loaded for the dropdown.
pub fn read(label: &str, hint: &str, p: &Painter, known: Receiver<Known>) -> Result<(String, Option<Known>)> {
    let mut st = State::new();
    let mut screen = Screen { p, label: label.to_string(), hint: hint.to_string(), above: 0 };
    let mut err = std::io::stderr();
    let raw = RawMode::on().map_err(io_err)?;
    let step = (|| -> std::io::Result<Step> {
        screen.draw(&mut st, &mut err, false)?;
        let mut pending: Option<Event> = None;
        loop {
            if st.known.is_none() {
                if let Ok(k) = known.try_recv() {
                    st.known = Some(k);
                    screen.draw(&mut st, &mut err, false)?;
                }
            }
            let ev = match pending.take() {
                Some(ev) => ev,
                None if event::poll(Duration::from_millis(120))? => event::read()?,
                None => continue,
            };
            if is_release(&ev) {
                continue;
            }
            let more = more_waiting(&mut pending)?;
            match st.handle(ev, more) {
                Step::Continue => {
                    if !more {
                        screen.draw(&mut st, &mut err, false)?;
                    }
                }
                done => {
                    st.cur = st.buf.len();
                    screen.draw(&mut st, &mut err, true)?;
                    write!(err, "\r\n")?;
                    return Ok(done);
                }
            }
        }
    })();
    drop(raw);
    match step.map_err(io_err)? {
        Step::Submit => Ok((st.text().trim().to_string(), st.known)),
        _ => Err(MagineError::new("Cancelled.", exit::INTERRUPTED, "interrupted")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn typed(st: &mut State, s: &str) {
        for c in s.chars() {
            st.handle(key(KeyCode::Char(c)), false);
        }
    }

    fn known() -> Option<Known> {
        let t = |kind, tag: &str| Taggable { kind, tag: tag.into(), name: tag.into(), id: tag.into(), detail: String::new() };
        Some((vec![t(Kind::Agent, "news-bot"), t(Kind::Agent, "price-watch"), t(Kind::File, "report.pdf"), t(Kind::File, "notes.md")], true))
    }

    #[test]
    fn at_and_hash_open_the_dropdown_and_enter_inserts_the_choice() {
        let mut st = State::new();
        st.known = known();
        typed(&mut st, "Summarise #rep");
        let (kind, _, query, list) = st.dropdown().unwrap();
        assert_eq!((kind, query.as_str(), list.len()), (Kind::File, "rep", 1));
        assert_eq!(st.handle(key(KeyCode::Enter), false), Step::Continue);
        assert_eq!(st.text(), "Summarise #report.pdf ");
        typed(&mut st, "for @");
        assert_eq!(st.dropdown().unwrap().3.len(), 2);
        st.handle(key(KeyCode::Down), false);
        st.handle(key(KeyCode::Tab), false);
        assert_eq!(st.text(), "Summarise #report.pdf for @price-watch ");
        assert_eq!(st.handle(key(KeyCode::Enter), false), Step::Submit);
    }

    #[test]
    fn esc_closes_the_dropdown_and_emails_do_not_open_it() {
        let mut st = State::new();
        st.known = known();
        typed(&mut st, "mail bob@ne");
        assert!(st.dropdown().is_none());
        st.buf.clear();
        st.cur = 0;
        typed(&mut st, "ask @ne");
        assert!(st.dropdown().is_some());
        st.handle(key(KeyCode::Esc), false);
        assert!(st.dropdown().is_none());
        assert_eq!(st.handle(key(KeyCode::Enter), false), Step::Submit);
        assert_eq!(st.text(), "ask @ne");
    }

    #[test]
    fn a_tag_in_the_middle_is_replaced_whole() {
        let mut st = State::new();
        st.known = known();
        typed(&mut st, "use #no today");
        st.cur = 7;
        st.handle(key(KeyCode::Tab), false);
        assert_eq!(st.text(), "use #notes.md today");
    }

    #[test]
    fn a_pasted_enter_does_not_submit_and_ctrl_c_cancels() {
        let mut st = State::new();
        typed(&mut st, "line one");
        assert_eq!(st.handle(key(KeyCode::Enter), true), Step::Continue);
        typed(&mut st, "line two");
        assert_eq!(st.text(), "line one line two");
        let ctrl_c = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(st.handle(ctrl_c, false), Step::Cancel);
        let altgr_at = Event::Key(KeyEvent::new(KeyCode::Char('@'), KeyModifiers::CONTROL | KeyModifiers::ALT));
        st.handle(altgr_at, false);
        assert!(st.text().ends_with('@'));
    }

    #[test]
    fn wide_letters_take_two_columns() {
        assert_eq!(width_of("ab".chars()), 2);
        assert_eq!(width_of("\u{4F60}\u{597D}".chars()), 4);
        assert_eq!(fit("\u{4F60}\u{597D}x", 3), "\u{4F60}");
    }
}
