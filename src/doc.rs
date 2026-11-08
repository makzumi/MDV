//! The document: markdown text, a selection (byte offsets), undo history, and
//! the formatting commands the toolbar/shortcuts invoke.

use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sel {
    pub anchor: usize,
    pub head: usize,
}

impl Sel {
    pub fn caret(at: usize) -> Self {
        Self { anchor: at, head: at }
    }
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

struct Snapshot {
    text: String,
    sel: Sel,
}

const HISTORY_LIMIT: usize = 500;
const COALESCE: Duration = Duration::from_millis(800);

pub struct Doc {
    pub text: String,
    pub sel: Sel,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    /// Bumped on every text change; used to invalidate caches.
    pub rev: u64,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: Option<Instant>,
}

impl Doc {
    pub fn new(text: String, path: Option<PathBuf>) -> Self {
        Self { text, sel: Sel::default(), path, dirty: false, rev: 0, undo: vec![], redo: vec![], last_edit: None }
    }

    pub fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into())
    }

    /// Record the current state before a change. `typing` edits made in quick
    /// succession are merged into a single undo step.
    fn checkpoint(&mut self, typing: bool) {
        let now = Instant::now();
        let merge = typing && self.last_edit.is_some_and(|t| now - t < COALESCE);
        if !merge {
            self.undo.push(Snapshot { text: self.text.clone(), sel: self.sel });
            if self.undo.len() > HISTORY_LIMIT {
                self.undo.remove(0);
            }
        }
        self.last_edit = typing.then_some(now);
        self.redo.clear();
    }

    fn changed(&mut self) {
        self.dirty = true;
        self.rev += 1;
    }

    /// Replace `range` with `with`, then set the selection.
    pub fn replace(&mut self, range: Range<usize>, with: &str, sel: Sel, typing: bool) {
        self.checkpoint(typing);
        self.text.replace_range(range, with);
        self.sel = self.clamp(sel);
        self.changed();
    }

    /// Replace the whole text (e.g. after the source view's editor changed it).
    pub fn set_text(&mut self, text: String, sel: Sel) {
        self.checkpoint(true);
        self.text = text;
        self.sel = self.clamp(sel);
        self.changed();
    }

    fn clamp(&self, s: Sel) -> Sel {
        let fix = |mut i: usize| {
            i = i.min(self.text.len());
            while !self.text.is_char_boundary(i) {
                i -= 1;
            }
            i
        };
        Sel { anchor: fix(s.anchor), head: fix(s.head) }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            let cur = Snapshot { text: std::mem::replace(&mut self.text, s.text), sel: self.sel };
            self.redo.push(cur);
            self.sel = self.clamp(s.sel);
            self.last_edit = None;
            self.changed();
        }
    }

    pub fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            let cur = Snapshot { text: std::mem::replace(&mut self.text, s.text), sel: self.sel };
            self.undo.push(cur);
            self.sel = self.clamp(s.sel);
            self.last_edit = None;
            self.changed();
        }
    }

    // -- Formatting commands --------------------------------------------------

    /// The selection for inline formatting: never starting inside a line's
    /// block prefix (`## `, `- `, `> `), which must stay at the line start.
    fn inline_range(&self) -> Range<usize> {
        let r = self.sel.range();
        let ls = self.text[..r.start].rfind('\n').map_or(0, |i| i + 1);
        let le = self.text[ls..].find('\n').map_or(self.text.len(), |i| ls + i);
        let line = &self.text[ls..le];
        let content = ls + (line.len() - strip_block_prefix(line).len());
        let start = r.start.max(content).min(r.end.max(content));
        start..r.end.max(start)
    }

    /// Toggle an inline wrapper like `**`, `*`, `~~`, `` ` ``.
    pub fn toggle_wrap(&mut self, mark: &str) {
        let r = self.inline_range();
        let m = mark.len();
        let t = &self.text;

        // Selection already includes the markers: **foo** → foo
        let inner = &t[r.clone()];
        if inner.len() >= 2 * m && inner.starts_with(mark) && inner.ends_with(mark) {
            let stripped = inner[m..inner.len() - m].to_owned();
            let sel = Sel { anchor: r.start, head: r.start + stripped.len() };
            return self.replace(r, &stripped, sel, false);
        }
        // Markers sit just outside the selection: **|foo|** → foo
        if r.start >= m && t[..r.start].ends_with(mark) && t[r.end..].starts_with(mark) {
            let inner = t[r.clone()].to_owned();
            let start = r.start - m;
            let sel = Sel { anchor: start, head: start + inner.len() };
            return self.replace(start..r.end + m, &inner, sel, false);
        }
        let inner = t[r.clone()].to_owned();
        let wrapped = format!("{mark}{inner}{mark}");
        let sel = Sel { anchor: r.start + m, head: r.start + m + inner.len() };
        self.replace(r, &wrapped, sel, false);
    }

    /// Turn the selection into a link to `url`, or insert a new link (using
    /// `text`, or the URL itself, as its text) at the caret.
    pub fn wrap_link(&mut self, url: &str, text: &str) {
        let r = self.sel.range();
        let label = if !r.is_empty() {
            self.text[r.clone()].to_owned()
        } else if text.is_empty() {
            crate::ops::escape_inline(url.trim_matches(['<', '>']), false)
        } else {
            crate::ops::escape_inline(text, false)
        };
        let s = format!("[{label}]({url})");
        let caret = r.start + s.len();
        self.replace(r, &s, Sel::caret(caret), false);
    }

    /// Color the selected text (`Some("#hex")`) or remove its color (`None`).
    /// Stored as `<span style="color: …">`; recoloring part of a colored run
    /// splits that span so tags never nest or overlap.
    pub fn set_color(&mut self, color: Option<&str>) {
        let r = self.inline_range();
        if r.is_empty() {
            return;
        }
        let cs = crate::ops::containers(&self.text);
        let wrap = |b: &str| match color {
            Some(h) => (format!("{}{b}</span>", crate::markdown::color_open_tag(h)), crate::markdown::color_open_tag(h).len()),
            None => (b.to_owned(), 0),
        };

        // Selection inside an existing color span: split it around the selection.
        let around = cs
            .iter()
            .filter(|c| c.html && c.open.end <= r.start && r.end <= c.close.start)
            .max_by_key(|c| c.open.start);
        if let Some(sp) = around {
            let t = &self.text;
            let (o, k) = (&t[sp.open.clone()], &t[sp.close.clone()]);
            let (a, b, c) = (&t[sp.open.end..r.start], &t[r.clone()], &t[r.end..sp.close.start]);
            let mut out = String::new();
            if !a.is_empty() {
                out.push_str(&format!("{o}{a}{k}"));
            }
            let (inner, lead) = wrap(b);
            let sel_start = sp.open.start + out.len() + lead;
            out.push_str(&inner);
            if !c.is_empty() {
                out.push_str(&format!("{o}{c}{k}"));
            }
            let sel = Sel { anchor: sel_start, head: sel_start + b.len() };
            return self.replace(sp.open.start..sp.close.end, &out, sel, false);
        }

        // Otherwise drop color spans fully inside the selection, then wrap.
        let mut drop: Vec<std::ops::Range<usize>> = cs
            .iter()
            .filter(|c| c.html && r.start <= c.open.start && c.close.end <= r.end)
            .flat_map(|c| [c.open.clone(), c.close.clone()])
            .collect();
        drop.sort_by_key(|d| d.start);
        let mut body = String::new();
        let mut pos = r.start;
        for d in &drop {
            body.push_str(&self.text[pos..d.start]);
            pos = d.end;
        }
        body.push_str(&self.text[pos..r.end]);
        let (out, lead) = wrap(&body);
        let sel = Sel { anchor: r.start + lead, head: r.start + lead + body.len() };
        self.replace(r, &out, sel, false);
    }

    /// Remove inline formatting (bold, italic, strikethrough, code, color)
    /// from the selection. Formatting that only partly overlaps it is split,
    /// so text outside the selection keeps its formatting. Links stay.
    pub fn clear_formatting(&mut self) {
        let r = self.inline_range();
        if r.is_empty() {
            return;
        }
        let (s, e) = (r.start, r.end);
        let cs = crate::ops::containers(&self.text);
        // Edits as (range to replace, insertion); applied together below.
        let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        let mut enclosing: Vec<&crate::ops::Container> = Vec::new();
        for c in &cs {
            let is_link = self.text[c.open.clone()].starts_with(['[', '<']) && !c.html;
            if is_link || c.close.end <= s || e <= c.open.start {
                continue;
            }
            let open_in = s <= c.open.start && c.open.end <= e;
            let close_in = s <= c.close.start && c.close.end <= e;
            let (open_t, close_t) = (self.text[c.open.clone()].to_owned(), self.text[c.close.clone()].to_owned());
            match (open_in, close_in) {
                (true, true) => {
                    edits.push((c.open.clone(), String::new()));
                    edits.push((c.close.clone(), String::new()));
                }
                (true, false) => {
                    // Starts inside the selection: move the opening marker to its end.
                    edits.push((c.open.clone(), String::new()));
                    edits.push((e..e, open_t));
                }
                (false, true) => {
                    edits.push((c.close.clone(), String::new()));
                    edits.push((s..s, close_t));
                }
                (false, false) if c.open.end <= s && e <= c.close.start => enclosing.push(c),
                _ => {}
            }
        }
        // Formatting around the whole selection: close before it, reopen after.
        enclosing.sort_by_key(|c| c.open.start);
        for c in enclosing.iter().rev() {
            let (open_t, close_t) = (self.text[c.open.clone()].to_owned(), self.text[c.close.clone()].to_owned());
            match (c.open.end == s, c.close.start == e) {
                (true, true) => {
                    edits.push((c.open.clone(), String::new()));
                    edits.push((c.close.clone(), String::new()));
                }
                (true, false) => {
                    edits.push((c.open.clone(), String::new()));
                    edits.push((e..e, open_t));
                }
                (false, true) => {
                    edits.push((c.close.clone(), String::new()));
                    edits.push((s..s, close_t));
                }
                (false, false) => {
                    edits.push((s..s, close_t));
                    edits.push((e..e, open_t));
                }
            }
        }
        if edits.is_empty() {
            return;
        }
        // Markdown markers must hug text: a closing marker can't follow a
        // space and an opening one can't precede one. So inserted closers at
        // `s` move left past spaces, inserted openers at `e` move right.
        let t = &self.text;
        let ws_left = t[..s].len() - t[..s].trim_end_matches([' ', '\t']).len();
        let ws_right = t[e..].len() - t[e..].trim_start_matches([' ', '\t']).len();
        for (r, _) in edits.iter_mut().filter(|(r, _)| r.is_empty()) {
            if r.start == s {
                *r = s - ws_left..s - ws_left;
            } else if r.start == e {
                *r = e + ws_right..e + ws_right;
            }
        }
        // Stable sort: insertions at the same point keep their push order
        // (closing markers innermost-first, opening markers outermost-first).
        edits.sort_by_key(|(r, _)| r.start);
        let lo = edits.iter().map(|(r, _)| r.start).min().unwrap_or(s);
        let hi = edits.iter().map(|(r, _)| r.end).max().unwrap_or(e);
        let mut out = String::new();
        let mut pos = lo;
        for (r, ins) in &edits {
            out.push_str(&self.text[pos..r.start]);
            out.push_str(ins);
            pos = r.end;
        }
        out.push_str(&self.text[pos..hi]);

        // New position of old offset `p`. Markers inserted exactly at `p`
        // count as before it when `inserts_before` (selection start).
        let map = |p: usize, inserts_before: bool| -> usize {
            let mut q = p as isize;
            for (r, ins) in &edits {
                if r.is_empty() {
                    if r.start < p || (r.start == p && inserts_before) {
                        q += ins.len() as isize;
                    }
                } else if r.end <= p {
                    q -= r.len() as isize;
                } else if r.start < p {
                    q -= (p - r.start) as isize;
                }
            }
            q as usize
        };
        let sel = Sel { anchor: map(s, true), head: map(e, false) };
        self.replace(lo..hi, &out, sel, false);
    }

    /// Byte range of the full lines touched by the selection.
    fn line_span(&self) -> Range<usize> {
        let r = self.sel.range();
        let start = self.text[..r.start].rfind('\n').map_or(0, |i| i + 1);
        let end = self.text[r.end..].find('\n').map_or(self.text.len(), |i| r.end + i);
        start..end
    }

    /// Apply `f` to each selected line; keeps the selection over those lines.
    fn map_lines(&mut self, f: impl Fn(usize, &str) -> String) {
        let span = self.line_span();
        let new: Vec<String> = self.text[span.clone()].split('\n').enumerate().map(|(i, l)| f(i, l)).collect();
        let new = new.join("\n");
        let sel = if self.sel.is_empty() {
            Sel::caret(span.start + new.len())
        } else {
            // Select the text, not the `## ` / `- ` prefix: a following inline
            // command (e.g. italic) must not wrap the prefix.
            let first = new.split('\n').next().unwrap_or("");
            let prefix = first.len() - strip_block_prefix(first).len();
            Sel { anchor: span.start + prefix, head: span.start + new.len() }
        };
        self.replace(span, &new, sel, false);
    }

    pub fn set_heading(&mut self, level: u8) {
        self.map_lines(|_, l| {
            let body = strip_block_prefix(l);
            if level == 0 { body.to_owned() } else { format!("{} {body}", "#".repeat(level as usize)) }
        });
    }

    /// Toggle a line prefix (`- `, `1. `, `- [ ] `, `> `) on the selected lines.
    pub fn toggle_prefix(&mut self, kind: Prefix) {
        let span = self.line_span();
        let all_have = self.text[span].split('\n').filter(|l| !l.trim().is_empty()).all(|l| kind.matches(l));
        self.map_lines(|i, l| {
            if l.trim().is_empty() && !all_have {
                return l.to_owned();
            }
            let body = strip_block_prefix(l);
            if all_have { body.to_owned() } else { format!("{}{body}", kind.text(i)) }
        });
    }

    pub fn insert_code_block(&mut self) {
        let span = self.line_span();
        let body = self.text[span.clone()].to_owned();
        let s = format!("```\n{body}\n```");
        let caret = span.start + 4 + body.len();
        self.replace(span, &s, Sel::caret(caret), false);
    }

    pub fn insert_rule(&mut self) {
        self.insert_block("---");
    }

    /// Insert `md` as its own block: on the caret's line if that line is
    /// blank, otherwise after it, separated by blank lines.
    pub fn insert_block(&mut self, md: &str) {
        let span = self.line_span();
        let blank = self.text[span.clone()].trim().is_empty();
        let (range, at) = if blank { (span.clone(), span.start) } else { (span.end..span.end, span.end) };
        let before = &self.text[..at];
        let after = &self.text[range.end..];
        let lead = if at == 0 || before.ends_with("\n\n") {
            ""
        } else if before.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        let trail = if after.is_empty() || after.starts_with("\n\n") {
            ""
        } else if after.starts_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        let s = format!("{lead}{md}{trail}");
        let caret = at + lead.len() + md.len();
        self.replace(range, &s, Sel::caret(caret), false);
    }

    /// Flip a task checkbox whose `[` is at byte `at`.
    pub fn toggle_task(&mut self, at: usize) {
        let Some(cur) = self.text.get(at..at + 3) else { return };
        let new = match cur {
            "[ ]" => "[x]",
            "[x]" | "[X]" => "[ ]",
            _ => return,
        };
        let sel = self.sel;
        self.replace(at..at + 3, new, sel, false);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prefix {
    Bullet,
    Number,
    Task,
    Quote,
}

impl Prefix {
    fn text(self, i: usize) -> String {
        match self {
            Prefix::Bullet => "- ".into(),
            Prefix::Number => format!("{}. ", i + 1),
            Prefix::Task => "- [ ] ".into(),
            Prefix::Quote => "> ".into(),
        }
    }
    fn matches(self, line: &str) -> bool {
        let l = line.trim_start();
        match self {
            Prefix::Task => l.starts_with("- [ ] ") || l.starts_with("- [x] ") || l.starts_with("- [X] "),
            Prefix::Bullet => {
                (l.starts_with("- ") || l.starts_with("* ") || l.starts_with("+ ")) && !Prefix::Task.matches(l)
            }
            Prefix::Number => ordered_prefix_len(l).is_some(),
            Prefix::Quote => l.starts_with('>'),
        }
    }
}

/// `"12. foo"` → `Some(4)`
pub fn ordered_prefix_len(l: &str) -> Option<usize> {
    let digits = l.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let rest = &l.as_bytes()[digits..];
    (rest.len() >= 2 && (rest[0] == b'.' || rest[0] == b')') && rest[1] == b' ').then_some(digits + 2)
}

/// Remove a leading heading / list / task / quote marker from a line.
pub fn strip_block_prefix(line: &str) -> &str {
    let l = line.trim_start();
    if let Some(rest) = l.strip_prefix('>') {
        return rest.strip_prefix(' ').unwrap_or(rest);
    }
    let hashes = l.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) && l[hashes..].starts_with(' ') {
        return &l[hashes + 1..];
    }
    let rest = if let Some(n) = ordered_prefix_len(l) {
        &l[n..]
    } else if l.starts_with("- ") || l.starts_with("* ") || l.starts_with("+ ") {
        &l[2..]
    } else {
        return line;
    };
    for t in ["[ ] ", "[x] ", "[X] "] {
        if let Some(r) = rest.strip_prefix(t) {
            return r;
        }
    }
    rest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(t: &str, a: usize, h: usize) -> Doc {
        let mut d = Doc::new(t.into(), None);
        d.sel = Sel { anchor: a, head: h };
        d
    }

    #[test]
    fn wrap_and_unwrap() {
        let mut d = doc("hello world", 6, 11);
        d.toggle_wrap("**");
        assert_eq!(d.text, "hello **world**");
        assert_eq!(&d.text[d.sel.range()], "world");
        d.toggle_wrap("**");
        assert_eq!(d.text, "hello world");
        d.undo();
        assert_eq!(d.text, "hello **world**");
        d.redo();
        assert_eq!(d.text, "hello world");
    }

    #[test]
    fn headings_and_lists() {
        let mut d = doc("- item", 3, 3);
        d.set_heading(2);
        assert_eq!(d.text, "## item");
        let mut d = doc("a\nb", 0, 3);
        d.toggle_prefix(Prefix::Number);
        assert_eq!(d.text, "1. a\n2. b");
        d.toggle_prefix(Prefix::Number);
        assert_eq!(d.text, "a\nb");
    }

    #[test]
    fn insert_block_spacing() {
        let mut d = doc("Hello world", 3, 3);
        d.insert_block("![](a.png)");
        assert_eq!(d.text, "Hello world\n\n![](a.png)");
        let mut d = doc("One\n\n\n\nTwo", 5, 5);
        d.insert_block("---");
        assert_eq!(d.text, "One\n\n---\n\nTwo");
        let mut d = doc("", 0, 0);
        d.insert_block("x");
        assert_eq!(d.text, "x");
    }

    #[test]
    fn links() {
        let mut d = doc("see docs here", 4, 8);
        d.wrap_link("https://x.org", "");
        assert_eq!(d.text, "see [docs](https://x.org) here");
        let mut d = doc("", 0, 0);
        d.wrap_link("https://x.org", "a*b");
        assert_eq!(d.text, "[a\\*b](https://x.org)");
    }

    #[test]
    fn colors() {
        let red = "#e5484d";
        let open = crate::markdown::color_open_tag(red);
        let mut d = doc("one two three", 4, 7);
        d.set_color(Some(red));
        assert_eq!(d.text, format!("one {open}two</span> three"));
        assert_eq!(&d.text[d.sel.range()], "two");

        // Recolor part of the run: the span splits.
        let blue = "#3e7bfa";
        let mut d = doc(&format!("{open}abc</span>"), open.len() + 1, open.len() + 2);
        d.set_color(Some(blue));
        let b = crate::markdown::color_open_tag(blue);
        assert_eq!(d.text, format!("{open}a</span>{b}b</span>{open}c</span>"));

        // Remove color from the whole run.
        let mut d = doc(&format!("x {open}abc</span> y"), 2 + open.len(), 2 + open.len() + 3);
        d.set_color(None);
        assert_eq!(d.text, "x abc y");
    }

    #[test]
    fn clear_formatting() {
        let check = |src: &str, sel: &str, want: &str| {
            let a = src.find(sel).unwrap();
            let mut d = doc(src, a, a + sel.len());
            d.clear_formatting();
            assert_eq!(d.text, want, "clearing {sel:?} in {src:?}");
            assert_eq!(&d.text[d.sel.range()], sel.replace(['*', '~', '`'], "").as_str());
        };
        check("a **bold** b", "**bold**", "a bold b");
        check("**one two three**", "two", "**one** two **three**");
        check("x *ab*cd y", "b*c", "x *a*bcd y");
        check("**ab**", "a", "a**b**");
        check("[**link**](u)", "**link**", "[link](u)");
        let red = crate::markdown::color_open_tag("#e5484d");
        let src = format!("{red}one two</span>");
        let a = src.find("two").unwrap();
        let mut d = doc(&src, a, a + 3);
        d.clear_formatting();
        assert_eq!(d.text, format!("{red}one</span> two"));
    }

    #[test]
    fn block_change_selects_text_not_prefix() {
        let mut d = doc("Features", 0, 8);
        d.set_heading(2);
        assert_eq!(&d.text[d.sel.range()], "Features");
        d.toggle_wrap("*");
        assert_eq!(d.text, "## *Features*");
    }

    #[test]
    fn inline_formats_skip_block_prefix() {
        // Selection starting at the line start, before "## ".
        let mut d = doc("## Features", 0, 11);
        d.toggle_wrap("*");
        assert_eq!(d.text, "## *Features*");
        let mut d = doc("- item", 0, 6);
        d.toggle_wrap("**");
        assert_eq!(d.text, "- **item**");
    }

    #[test]
    fn task_toggle() {
        let mut d = doc("- [ ] x", 0, 0);
        d.toggle_task(2);
        assert_eq!(d.text, "- [x] x");
    }
}
