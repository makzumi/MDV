//! Markdown → display model.
//!
//! The markdown *source string* is the single source of truth. This module
//! derives two things from it, both carrying byte offsets back into the source:
//!
//! * [`parse_blocks`]: top-level blocks with "rendered" rows (markers removed),
//!   used to draw blocks that are not being edited and to map clicks back to a
//!   source position.
//! * [`highlight`]: style runs over the *raw* source (markers kept but dimmed),
//!   used for the block being edited and for the full source view.

use crate::images::{self, ImageRef};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::ops::Range;

pub fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

// ---------------------------------------------------------------------------
// Blocks / rendered rows
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    Heading(u8),
    List,
    Quote,
    Code,
    Rule,
    Table,
    Html,
    Other,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub html: bool,
    pub link: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Span {
    pub text: String,
    pub style: Style,
    /// Byte offset in the source where this text starts.
    pub src: usize,
    /// `true` if `text` is a verbatim copy of `source[src..src + text.len()]`,
    /// so each char maps exactly. Otherwise every char maps to `src`.
    pub exact: bool,
    /// Set for image spans (`text` is empty); drawn as a picture, not text.
    pub image: Option<Box<ImageRef>>,
}

#[derive(Clone, Debug)]
pub enum Marker {
    Bullet(usize),
    Number(u64),
    Task { checked: bool, at: usize },
}

#[derive(Clone, Debug)]
pub struct TableRow {
    pub header: bool,
    pub cells: Vec<Vec<Span>>,
}

#[derive(Clone, Debug)]
pub enum RowKind {
    Text,
    Code,
    Rule,
    Table(Vec<TableRow>),
}

#[derive(Clone, Debug)]
pub struct Row {
    pub kind: RowKind,
    pub heading: u8,
    pub list_depth: usize,
    pub quote_depth: usize,
    pub marker: Option<Marker>,
    /// Centered (from HTML `align="center"`).
    pub center: bool,
    pub spans: Vec<Span>,
    /// Fallback source position for clicks on an empty row.
    pub src: usize,
}

#[derive(Clone, Debug)]
pub struct Block {
    /// Byte range in the source, trailing whitespace trimmed.
    pub range: Range<usize>,
    pub kind: BlockKind,
    pub rows: Vec<Row>,
}

fn kind_of(tag: &Tag) -> BlockKind {
    match tag {
        Tag::Paragraph => BlockKind::Paragraph,
        Tag::Heading { level, .. } => BlockKind::Heading(*level as u8),
        Tag::List(_) => BlockKind::List,
        Tag::BlockQuote(_) => BlockKind::Quote,
        Tag::CodeBlock(_) => BlockKind::Code,
        Tag::Table(_) => BlockKind::Table,
        Tag::HtmlBlock => BlockKind::Html,
        _ => BlockKind::Other,
    }
}

struct Builder<'a> {
    src: &'a str,
    blocks: Vec<Block>,
    depth: usize,
    block_start: usize,
    block_kind: BlockKind,
    rows: Vec<Row>,
    row: Option<Row>,
    bold: u32,
    italic: u32,
    strike: u32,
    /// Image being parsed (`![alt](src)`): its alt text accumulates here.
    cur_image: Option<ImageRef>,
    html: bool,
    links: Vec<String>,
    lists: Vec<Option<u64>>,
    quote: usize,
}

impl<'a> Builder<'a> {
    fn style(&self) -> Style {
        Style {
            bold: self.bold > 0,
            italic: self.italic > 0,
            strike: self.strike > 0,
            code: false,
            html: self.html,
            link: self.links.last().cloned(),
        }
    }

    fn new_row(&mut self, src: usize) -> &mut Row {
        self.flush();
        self.row = Some(Row {
            kind: RowKind::Text,
            heading: 0,
            list_depth: self.lists.len(),
            quote_depth: self.quote,
            marker: None,
            center: false,
            spans: Vec::new(),
            src,
        });
        self.row.as_mut().unwrap()
    }

    fn flush(&mut self) {
        if let Some(row) = self.row.take() {
            let empty = matches!(row.kind, RowKind::Text) && row.spans.is_empty() && row.marker.is_none();
            if !empty {
                self.rows.push(row);
            }
        }
    }

    fn push_span(&mut self, text: String, style: Style, range: &Range<usize>, exact_hint: Option<usize>) {
        let (src, exact) = match exact_hint {
            Some(off) => (off, true),
            None => {
                let exact = self.src.get(range.clone()) == Some(text.as_str());
                (range.start, exact)
            }
        };
        self.push(Span { text, style, src, exact, image: None }, range.start);
    }

    fn push_image(&mut self, img: ImageRef) {
        let src = img.range.start;
        self.push(Span { text: String::new(), style: Style::default(), src, exact: false, image: Some(Box::new(img)) }, src);
    }

    fn push(&mut self, span: Span, at: usize) {
        if self.row.is_none() {
            self.new_row(at);
        }
        let row = self.row.as_mut().unwrap();
        if let RowKind::Table(trows) = &mut row.kind {
            if let Some(cell) = trows.last_mut().and_then(|r| r.cells.last_mut()) {
                cell.push(span);
            }
        } else {
            row.spans.push(span);
        }
    }

    fn start(&mut self, tag: &Tag, range: &Range<usize>) {
        match tag {
            Tag::Paragraph => {
                // A list item already opened a row (with its bullet); reuse it.
                let reuse = matches!(&self.row, Some(r) if r.spans.is_empty() && matches!(r.kind, RowKind::Text));
                if !reuse {
                    self.new_row(range.start);
                }
            }
            Tag::Heading { level, .. } => {
                self.new_row(range.start).heading = *level as u8;
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote += 1;
            }
            Tag::CodeBlock(_) => {
                self.new_row(range.start).kind = RowKind::Code;
            }
            Tag::HtmlBlock => {
                self.new_row(range.start);
                self.html = true;
            }
            Tag::List(start) => {
                self.flush();
                self.lists.push(*start);
            }
            Tag::Item => {
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let m = Marker::Number(*n);
                        *n += 1;
                        m
                    }
                    _ => Marker::Bullet(self.lists.len().saturating_sub(1)),
                };
                self.new_row(range.start).marker = Some(marker);
            }
            Tag::Table(_) => {
                self.new_row(range.start).kind = RowKind::Table(Vec::new());
            }
            Tag::TableHead | Tag::TableRow => {
                let header = matches!(tag, Tag::TableHead);
                if let Some(Row { kind: RowKind::Table(rows), .. }) = &mut self.row {
                    rows.push(TableRow { header, cells: Vec::new() });
                }
            }
            Tag::TableCell => {
                if let Some(Row { kind: RowKind::Table(rows), .. }) = &mut self.row
                    && let Some(r) = rows.last_mut()
                {
                    r.cells.push(Vec::new());
                }
            }
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } => self.links.push(dest_url.to_string()),
            Tag::Image { dest_url, .. } => {
                self.cur_image =
                    Some(ImageRef { src: dest_url.to_string(), alt: String::new(), width: None, range: range.clone() });
            }
            Tag::FootnoteDefinition(_) | Tag::DefinitionList | Tag::DefinitionListTitle | Tag::DefinitionListDefinition => {
                self.new_row(range.start);
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: &TagEnd, range: &Range<usize>) {
        match tag {
            TagEnd::CodeBlock => {
                if let Some(row) = &mut self.row
                    && let Some(last) = row.spans.last_mut()
                {
                    while last.text.ends_with('\n') {
                        last.text.pop();
                    }
                }
                self.flush();
            }
            TagEnd::HtmlBlock => {
                // Nothing visible (only tags)? Show the raw HTML so the block
                // isn't invisible and can still be clicked/edited.
                if self.row.as_ref().is_none_or(|r| r.spans.is_empty()) {
                    let raw = self.src[range.clone()].trim_end().to_owned();
                    let style = Style { html: true, ..Style::default() };
                    self.push_span(raw, style, &(range.start..range.start), None);
                }
                self.flush();
                self.html = false;
            }
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item | TagEnd::Table => self.flush(),
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => {
                self.links.pop();
            }
            TagEnd::Image => {
                if let Some(img) = self.cur_image.take() {
                    self.push_image(img);
                }
            }
            _ => {}
        }
    }

    fn inline(&mut self, ev: Event, range: Range<usize>) {
        match ev {
            Event::Text(t) | Event::Code(t) if self.cur_image.is_some() => {
                self.cur_image.as_mut().unwrap().alt.push_str(&t);
            }
            Event::Text(t) => self.push_span(t.into_string(), self.style(), &range, None),
            Event::Code(t) => {
                let off = self.src.get(range.clone()).and_then(|s| s.find(&*t)).map(|o| range.start + o);
                let mut style = self.style();
                style.code = true;
                self.push_span(t.into_string(), style, &range, off);
            }
            Event::Html(t) => self.html_chunk(&t, range, true),
            Event::InlineHtml(t) => self.html_chunk(&t, range, false),
            Event::SoftBreak => self.push_span(" ".into(), self.style(), &(range.start..range.start), None),
            Event::HardBreak => self.push_span("\n".into(), self.style(), &(range.start..range.start), None),
            Event::TaskListMarker(checked) => {
                if let Some(row) = &mut self.row {
                    row.marker = Some(Marker::Task { checked, at: range.start });
                }
            }
            Event::FootnoteReference(l) => self.push_span(format!("[{l}]"), self.style(), &range, None),
            Event::Rule => {
                self.new_row(range.start).kind = RowKind::Rule;
                self.flush();
            }
            _ => {}
        }
    }

    /// HTML: `<img>` tags become images. Inside HTML blocks, other tags are
    /// dropped and their text shown plainly (mapped word by word back to the
    /// source); unknown inline tags stay visible as code.
    fn html_chunk(&mut self, t: &str, range: Range<usize>, block: bool) {
        let tags = images::find_img_tags(t);
        let exact = self.src.get(range.clone()) == Some(t);

        if block {
            let (heading, center) = images::block_hints(t);
            if let Some(row) = &mut self.row {
                if heading > 0 {
                    row.heading = heading;
                }
                row.center |= center;
            }
        } else if tags.is_empty() {
            let style = Style { html: true, ..self.style() };
            self.push_span(t.to_owned(), style, &range, None);
            return;
        }

        let mut last = 0;
        for tag in tags {
            if block {
                self.push_words(&t[last..tag.range.start], range.start + last, exact);
            }
            let r = if exact { range.start + tag.range.start..range.start + tag.range.end } else { range.clone() };
            self.push_image(ImageRef { src: tag.src, alt: tag.alt, width: tag.width, range: r });
            last = tag.range.end;
        }
        if block {
            self.push_words(&t[last..], range.start + last, exact);
        }
    }

    /// Visible words of an HTML fragment starting at source offset `at`.
    fn push_words(&mut self, html: &str, at: usize, exact: bool) {
        let style = Style { html: false, ..self.style() };
        for (r, word) in images::visible_words(html) {
            let src = at + r.start;
            let needs_space = self
                .row
                .as_ref()
                .and_then(|row| row.spans.last())
                .is_some_and(|s| s.image.is_none() && !s.text.ends_with([' ', '\n']));
            if needs_space {
                self.push_span(" ".into(), style.clone(), &(src..src), None);
            }
            let hint = (exact && word == html[r.clone()]).then_some(src);
            self.push_span(word, style.clone(), &(src..src), hint);
        }
    }

    fn finish_block(&mut self, end: usize) {
        self.flush();
        let e = trim_block_end(self.src, self.block_start, end);
        self.blocks.push(Block {
            range: self.block_start..e,
            kind: self.block_kind,
            rows: std::mem::take(&mut self.rows),
        });
    }
}

/// Drop trailing newlines and whitespace-only lines, but keep trailing spaces
/// on the last real line: an empty list item `- ` must keep its space or the
/// caret after it would fall outside the block.
fn trim_block_end(src: &str, start: usize, end: usize) -> usize {
    let s = &src[start..end.min(src.len())];
    let mut e = s.len();
    loop {
        let t = s[..e].trim_end_matches(['\n', '\r']);
        let line_start = t.rfind('\n').map_or(0, |i| i + 1);
        if line_start > 0 && t[line_start..].trim().is_empty() {
            e = line_start;
            continue;
        }
        return start + t.len();
    }
}

pub fn parse_blocks(src: &str) -> Vec<Block> {
    let mut b = Builder {
        src,
        blocks: Vec::new(),
        depth: 0,
        block_start: 0,
        block_kind: BlockKind::Other,
        rows: Vec::new(),
        row: None,
        bold: 0,
        italic: 0,
        strike: 0,
        cur_image: None,
        html: false,
        links: Vec::new(),
        lists: Vec::new(),
        quote: 0,
    };

    for (ev, range) in Parser::new_ext(src, options()).into_offset_iter() {
        match ev {
            Event::Start(tag) => {
                if b.depth == 0 {
                    b.block_start = range.start;
                    b.block_kind = kind_of(&tag);
                }
                b.depth += 1;
                b.start(&tag, &range);
            }
            Event::End(tag) => {
                b.end(&tag, &range);
                b.depth = b.depth.saturating_sub(1);
                if b.depth == 0 {
                    b.finish_block(range.end);
                }
            }
            other => {
                let top = b.depth == 0;
                if top {
                    b.block_start = range.start;
                    b.block_kind = if matches!(other, Event::Rule) { BlockKind::Rule } else { BlockKind::Other };
                }
                let end = range.end;
                b.inline(other, range);
                if top {
                    b.finish_block(end);
                }
            }
        }
    }
    b.blocks
}

// ---------------------------------------------------------------------------
// Source highlighting
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hl {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub code_block: bool,
    pub link: bool,
    pub heading: u8,
    pub quote: bool,
    pub html: bool,
    /// Markdown syntax (not content): `**`, `#`, `- `, `](url)`, fences…
    pub marker: bool,
}

/// Style runs covering every byte of `src`, in order.
pub fn highlight(src: &str) -> Vec<(Range<usize>, Hl)> {
    let n = src.len();
    let mut styles = vec![Hl::default(); n];
    let mut content = vec![false; n];

    let mut apply = |r: &Range<usize>, f: &dyn Fn(&mut Hl)| {
        for s in &mut styles[r.start.min(n)..r.end.min(n)] {
            f(s);
        }
    };

    for (ev, r) in Parser::new_ext(src, options()).into_offset_iter() {
        match &ev {
            Event::Start(tag) => match tag {
                Tag::Strong => apply(&r, &|s| s.bold = true),
                Tag::Emphasis => apply(&r, &|s| s.italic = true),
                Tag::Strikethrough => apply(&r, &|s| s.strike = true),
                Tag::Link { .. } | Tag::Image { .. } => apply(&r, &|s| s.link = true),
                Tag::Heading { level, .. } => {
                    let l = *level as u8;
                    apply(&r, &|s| s.heading = l)
                }
                Tag::CodeBlock(_) => apply(&r, &|s| s.code_block = true),
                Tag::BlockQuote(_) => apply(&r, &|s| s.quote = true),
                Tag::HtmlBlock => apply(&r, &|s| s.html = true),
                _ => {}
            },
            Event::Text(_) | Event::SoftBreak | Event::HardBreak => {
                content[r.start.min(n)..r.end.min(n)].fill(true);
            }
            Event::Code(t) => {
                apply(&r, &|s| s.code = true);
                if let Some(off) = src.get(r.clone()).and_then(|s| s.find(&**t)) {
                    let a = r.start + off;
                    content[a..(a + t.len()).min(n)].fill(true);
                }
            }
            Event::Html(_) | Event::InlineHtml(_) => {
                apply(&r, &|s| s.html = true);
                content[r.start.min(n)..r.end.min(n)].fill(true);
            }
            _ => {}
        }
    }

    let bytes = src.as_bytes();
    let mut runs: Vec<(Range<usize>, Hl)> = Vec::new();
    for (i, _) in src.char_indices() {
        let mut s = styles[i];
        s.marker = !content[i] && !bytes[i].is_ascii_whitespace();
        match runs.last_mut() {
            Some((r, last)) if *last == s => r.end = i,
            _ => runs.push((i..i, s)),
        }
        // Extend to the end of this char.
        let len = src[i..].chars().next().map_or(1, char::len_utf8);
        runs.last_mut().unwrap().0.end = i + len;
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_have_trimmed_ranges() {
        let src = "# Title\n\nHello **world**\n\n- a\n- b\n\n```\ncode\n```\n";
        let b = parse_blocks(src);
        let kinds: Vec<_> = b.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, vec![BlockKind::Heading(1), BlockKind::Paragraph, BlockKind::List, BlockKind::Code]);
        assert_eq!(&src[b[0].range.clone()], "# Title");
        assert_eq!(&src[b[1].range.clone()], "Hello **world**");
        assert_eq!(&src[b[2].range.clone()], "- a\n- b");
        assert_eq!(&src[b[3].range.clone()], "```\ncode\n```");
        let src = "- a\n- \n   \n\nNext";
        assert_eq!(&src[parse_blocks(src)[0].range.clone()], "- a\n- ");
    }

    #[test]
    fn spans_map_to_source() {
        let src = "Hello **world**";
        let b = parse_blocks(src);
        let spans = &b[0].rows[0].spans;
        assert_eq!(spans[1].text, "world");
        assert!(spans[1].exact);
        assert_eq!(spans[1].src, 8);
        assert!(spans[1].style.bold);
    }

    #[test]
    fn list_rows_and_tasks() {
        let src = "- [ ] todo\n- [x] done\n1. one\n";
        let b = parse_blocks(src);
        let rows = &b[0].rows;
        assert!(matches!(rows[0].marker, Some(Marker::Task { checked: false, at: 2 })));
        assert!(matches!(rows[1].marker, Some(Marker::Task { checked: true, .. })));
    }

    #[test]
    fn images_markdown_and_html() {
        let src = "![Logo](img/a.png)\n\n<p align=\"center\">\n  <img src=\"b.png\" width=\"50%\">\n</p>\n\nText <img src=\"c.png\" width=\"20\"> here";
        let b = parse_blocks(src);
        let img = |bi: usize| -> Vec<ImageRef> {
            b[bi].rows.iter().flat_map(|r| r.spans.iter()).filter_map(|s| s.image.as_deref().cloned()).collect()
        };
        let a = img(0);
        assert_eq!((a[0].src.as_str(), a[0].alt.as_str()), ("img/a.png", "Logo"));
        assert_eq!(&src[a[0].range.clone()], "![Logo](img/a.png)");
        let h = img(1);
        assert_eq!(h[0].width, Some(images::Width::Percent(50.0)));
        assert_eq!(&src[h[0].range.clone()], "<img src=\"b.png\" width=\"50%\">");
        // The <p> wrapper is dropped, not shown as text; the block is centered.
        assert!(b[1].rows[0].center);
        assert!(b[1].rows.iter().flat_map(|r| &r.spans).all(|s| s.image.is_some()));
        let i = img(2);
        assert_eq!(&src[i[0].range.clone()], "<img src=\"c.png\" width=\"20\">");
    }

    #[test]
    fn highlight_marks_syntax() {
        let src = "a **b**";
        let runs = highlight(src);
        let marker_text: String = runs
            .iter()
            .filter(|(_, s)| s.marker)
            .map(|(r, _)| &src[r.clone()])
            .collect();
        assert_eq!(marker_text, "****");
        assert_eq!(runs.iter().map(|(r, _)| r.len()).sum::<usize>(), src.len());
    }
}
