//! Pure source transformations behind WYSIWYG editing.
//!
//! The editor never shows markdown syntax, so edits made on rendered text must
//! be translated into source edits that keep the markup valid:
//!
//! * [`expand_deletions`]: deleting text must not leave half a `**` pair or an
//!   empty `****` behind, so partially-deleted inline containers lose both
//!   markers.
//! * [`enclosing`]: splitting a paragraph inside bold/italic/links must close
//!   the formatting before the break and reopen it after.
//! * [`escape_inline`]: typed characters are literal text, so markdown-special
//!   characters are backslash-escaped.

use pulldown_cmark::{Event, Parser, Tag};
use std::ops::Range;

/// An inline formatting span in the source: `**…**`, `*…*`, `~~…~~`,
/// `` `…` ``, `[…](…)`, `<…>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub open: Range<usize>,
    pub close: Range<usize>,
    pub code: bool,
    /// An HTML `<span>`/`<font>` pair (text color).
    pub html: bool,
}

/// Leading run of `ch` at the start of `s` (at most `max`).
fn run_len(s: &str, ch: char, max: usize) -> usize {
    s.chars().take_while(|c| *c == ch).take(max).count() * ch.len_utf8()
}

pub fn containers(src: &str) -> Vec<Container> {
    let mut out = Vec::new();
    let mut open_spans: Vec<Range<usize>> = Vec::new();
    for (ev, r) in Parser::new_ext(src, crate::markdown::options()).into_offset_iter() {
        if let Event::InlineHtml(t) = &ev {
            if crate::markdown::span_open_color(t).is_some() {
                open_spans.push(r.clone());
            } else if crate::markdown::is_span_close(t)
                && let Some(open) = open_spans.pop()
            {
                out.push(Container { open, close: r.clone(), code: false, html: true });
            }
            continue;
        }
        let s = &src[r.clone()];
        let pair = |k: usize| -> Option<Container> {
            (k > 0 && 2 * k <= s.len()).then(|| Container {
                open: r.start..r.start + k,
                close: r.end - k..r.end,
                code: false,
                html: false,
            })
        };
        let c = match &ev {
            Event::Start(Tag::Strong) => {
                let ch = s.chars().next().unwrap_or('*');
                pair(run_len(s, ch, 2).min(2))
            }
            Event::Start(Tag::Emphasis) => pair(1),
            Event::Start(Tag::Strikethrough) => pair(run_len(s, '~', 2)),
            Event::Code(_) => pair(run_len(s, '`', usize::MAX)).map(|c| Container { code: true, ..c }),
            Event::Start(Tag::Link { .. }) if s.starts_with('<') => pair(1),
            Event::Start(Tag::Link { .. }) if s.starts_with('[') => s.rfind("](").map(|i| Container {
                open: r.start..r.start + 1,
                close: r.start + i..r.end,
                code: false,
                html: false,
            }),
            _ => None,
        };
        out.extend(c);
    }
    out
}

/// Sort and merge overlapping/adjacent ranges.
fn normalize(mut v: Vec<Range<usize>>) -> Vec<Range<usize>> {
    v.retain(|r| r.start < r.end);
    v.sort_by_key(|r| r.start);
    let mut out: Vec<Range<usize>> = Vec::with_capacity(v.len());
    for r in v {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

fn covers(dels: &[Range<usize>], r: &Range<usize>) -> bool {
    dels.iter().any(|d| d.start <= r.start && r.end <= d.end)
}

/// Grow `dels` so no container is left with only one marker, and containers
/// whose content is entirely deleted disappear completely.
pub fn expand_deletions(cs: &[Container], dels: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut dels = normalize(dels);
    for _ in 0..4 {
        let mut add = Vec::new();
        for c in cs {
            let (o, cl) = (covers(&dels, &c.open), covers(&dels, &c.close));
            let content = c.open.end..c.close.start;
            let content_gone = content.is_empty() || covers(&dels, &content);
            match (o, cl) {
                (true, false) => add.push(c.close.clone()),
                (false, true) => add.push(c.open.clone()),
                (false, false) if content_gone && dels.iter().any(|d| d.start < c.close.end && c.open.start < d.end) => {
                    add.push(c.open.clone());
                    add.push(c.close.clone());
                }
                _ => {}
            }
        }
        if add.is_empty() {
            break;
        }
        dels.extend(add);
        dels = normalize(dels);
    }
    dels
}

/// Where byte `p` ends up after removing `dels` (sorted, disjoint).
pub fn shift(p: usize, dels: &[Range<usize>]) -> usize {
    let mut removed = 0;
    for d in dels {
        if d.end <= p {
            removed += d.len();
        } else if d.start < p {
            removed += p - d.start;
        }
    }
    p - removed
}

/// One replacement equivalent to removing all `dels`: (span, replacement).
pub fn apply_deletions(text: &str, dels: &[Range<usize>]) -> (Range<usize>, String) {
    let (Some(first), Some(last)) = (dels.first(), dels.last()) else { return (0..0, String::new()) };
    let span = first.start..last.end;
    let mut out = String::new();
    let mut pos = span.start;
    for d in dels {
        out.push_str(&text[pos..d.start]);
        pos = d.end;
    }
    (span, out)
}

/// Containers around byte `p`: (closing markers innermost-first, opening
/// markers outermost-first, inside a code span).
pub fn enclosing(src: &str, cs: &[Container], p: usize) -> (String, String, bool) {
    let mut around: Vec<&Container> = cs.iter().filter(|c| c.open.end <= p && p <= c.close.start).collect();
    around.sort_by_key(|c| c.open.start);
    let in_code = around.iter().any(|c| c.code);
    let open: String = around.iter().map(|c| &src[c.open.clone()]).collect();
    let close: String = around.iter().rev().map(|c| &src[c.close.clone()]).collect();
    (close, open, in_code)
}

/// Escape characters that would otherwise turn typed text into markup.
pub fn escape_inline(t: &str, in_table: bool) -> String {
    let mut out = String::with_capacity(t.len());
    for ch in t.chars() {
        if matches!(ch, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '~') || (in_table && ch == '|') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delete(src: &str, r: Range<usize>) -> String {
        let cs = containers(src);
        let dels = expand_deletions(&cs, vec![r]);
        let (span, repl) = apply_deletions(src, &dels);
        let mut s = src.to_owned();
        s.replace_range(span, &repl);
        s
    }

    #[test]
    fn finds_containers() {
        let src = "a **b** *c* ~~d~~ `e` [f](u)";
        let cs = containers(src);
        let marks: Vec<(&str, &str)> = cs.iter().map(|c| (&src[c.open.clone()], &src[c.close.clone()])).collect();
        assert_eq!(marks, [("**", "**"), ("*", "*"), ("~~", "~~"), ("`", "`"), ("[", "](u)")]);
    }

    #[test]
    fn deleting_all_content_removes_markers() {
        assert_eq!(delete("a **b** c", 4..5), "a  c");
        assert_eq!(delete("x [link](http://u) y", 3..7), "x  y");
    }

    #[test]
    fn deleting_one_marker_removes_its_pair() {
        // Selection from inside plain text into the middle of bold.
        assert_eq!(delete("ab **cd** e", 1..6), "ad e");
    }

    #[test]
    fn partial_content_keeps_markers() {
        assert_eq!(delete("**bold**", 2..3), "**old**");
    }

    #[test]
    fn color_spans_are_containers() {
        let src = "a <span style=\"color: red\">x</span> b";
        let cs = containers(src);
        assert_eq!(cs.len(), 1);
        assert!(cs[0].html);
        let x = src.find('x').unwrap();
        assert_eq!(delete(src, x..x + 1), "a  b");
        let (close, open, _) = enclosing(src, &cs, x);
        assert_eq!((close.as_str(), open.as_str()), ("</span>", "<span style=\"color: red\">"));
    }

    #[test]
    fn shift_positions() {
        let dels = vec![1..3, 5..6];
        assert_eq!(shift(0, &dels), 0);
        assert_eq!(shift(2, &dels), 1);
        assert_eq!(shift(4, &dels), 2);
        assert_eq!(shift(10, &dels), 7);
    }

    #[test]
    fn enclosing_markers() {
        let src = "**a [b|c](u) d**";
        let p = src.find('|').unwrap();
        let (close, open, code) = enclosing(src, &containers(src), p);
        assert_eq!((close.as_str(), open.as_str(), code), ("](u)**", "**[", false));
    }

    #[test]
    fn escaping() {
        assert_eq!(escape_inline("a*b_c [x] `y`", false), "a\\*b\\_c \\[x\\] \\`y\\`");
        assert_eq!(escape_inline("a|b", true), "a\\|b");
        assert_eq!(escape_inline("a|b", false), "a|b");
    }
}
