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

    /// Toggle an inline wrapper like `**`, `*`, `~~`, `` ` ``.
    pub fn toggle_wrap(&mut self, mark: &str) {
        let r = self.sel.range();
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

    pub fn insert_link(&mut self) {
        let r = self.sel.range();
        let label = self.text[r.clone()].to_owned();
        let label = if label.is_empty() { "link".to_owned() } else { label };
        let s = format!("[{label}](https://)");
        let url_start = r.start + label.len() + 3;
        let sel = Sel { anchor: url_start, head: url_start + "https://".len() };
        self.replace(r, &s, sel, false);
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
            Sel { anchor: span.start, head: span.start + new.len() }
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

#[derive(Clone, Copy, PartialEq, Eq)]
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
    fn task_toggle() {
        let mut d = doc("- [ ] x", 0, 0);
        d.toggle_task(2);
        assert_eq!(d.text, "- [x] x");
    }
}
