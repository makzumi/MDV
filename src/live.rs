//! The WYSIWYG "Editor" view.
//!
//! Everything is drawn rendered; markdown syntax is never shown. The markdown
//! text in [`Doc`] stays the single source of truth: every rendered character
//! knows the source byte range it came from, so caret moves, selection,
//! typing and deleting on the rendered text are translated into source edits
//! (with the help of [`crate::ops`], which keeps the markup balanced).
//!
//! The caret/selection are stored in `doc.sel` as source byte offsets, shared
//! with the Source view and the toolbar commands.

use crate::doc::{Doc, Prefix, Sel, ordered_prefix_len, strip_block_prefix};
use crate::editor::{Marks, View, marks_in};
use crate::images::{self, ImageRef, Width};
use crate::markdown::{self, Block, BlockKind, Marker, RowKind, Span};
use crate::ops;
use crate::theme::{BASE, MONO, Theme};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, Event, EventFilter, FontId, Id, Key, Modifiers, Pos2, Rect,
    Sense, Stroke, Ui, Vec2,
    text::{CCursor, LayoutJob},
};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

pub const LIVE_ID: &str = "mdv-live";

/// A formatting command chosen in the floating selection toolbar; the app
/// applies it like the matching main-toolbar button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Heading(u8),
    Prefix(Prefix),
    Wrap(&'static str),
    Link,
    /// Text color (`None` = default).
    Color(Option<&'static str>),
    Clear,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BubbleMenu {
    Closed,
    Block,
    Color,
}

/// A row of color swatches plus "Default". Returns the pick:
/// `Some(Some(hex))`, `Some(None)` for default, or `None` if nothing clicked.
pub fn color_picker(ui: &mut Ui, theme: &Theme, current: Option<[u8; 3]>) -> Option<Option<&'static str>> {
    let mut pick = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let default = egui::Button::new(egui::RichText::new("Default").size(13.0))
            .selected(current.is_none())
            .min_size(Vec2::new(0.0, 24.0));
        if ui.add(default).on_hover_text("Remove color").clicked() {
            pick = Some(None);
        }
        for (name, hex) in markdown::COLORS {
            let rgb = markdown::parse_color(hex).unwrap_or([128, 128, 128]);
            let color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
            let painter = ui.painter();
            painter.circle_filled(rect.center(), 9.0, color);
            if current == Some(rgb) {
                painter.circle_stroke(rect.center(), 11.0, Stroke::new(2.0, theme.text));
            } else if resp.hovered() {
                painter.circle_stroke(rect.center(), 11.0, Stroke::new(1.5, theme.faint));
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.on_hover_text(name).clicked() {
                pick = Some(Some(hex));
            }
        }
    });
    pick
}

/// The "A" text-color button glyph with an underline in the current color.
pub fn color_button(ui: &mut Ui, theme: &Theme, current: Option<[u8; 3]>, selected: bool) -> egui::Response {
    let b = egui::Button::new(egui::RichText::new(egui_phosphor::regular::TEXT_A_UNDERLINE).size(16.0))
        .frame_when_inactive(selected)
        .selected(selected)
        .min_size(Vec2::splat(28.0));
    let resp = ui.add(b);
    let bar = Rect::from_center_size(resp.rect.center_bottom() - Vec2::new(0.0, 5.0), Vec2::new(14.0, 3.0));
    let c = current.map_or(theme.faint, |[r, g, b]| Color32::from_rgb(r, g, b));
    ui.painter().rect_filled(bar, 1.0, c);
    resp
}
const BLINK: f64 = 0.53;

// ---------------------------------------------------------------------------
// Visual rows: the laid-out text the caret can live in
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowK {
    /// Paragraph, heading, list item, quote line.
    Flow,
    /// Code block content (typed text is inserted raw).
    Code,
    /// Table cell.
    Cell,
    /// Empty line shown where the caret sits between blocks.
    Phantom,
}

struct VRow {
    galley: Arc<egui::Galley>,
    /// Galley origin on screen.
    pos: Pos2,
    /// Hit-test area.
    rect: Rect,
    /// Per rendered char: source byte range it came from.
    starts: Vec<usize>,
    ends: Vec<usize>,
    /// Insertion point when the row has no chars.
    empty_ins: usize,
    kind: RowK,
    /// Index of the block this row belongs to (for phantoms: the next block).
    block: usize,
    links: Vec<(Range<usize>, String)>,
}

impl VRow {
    fn n(&self) -> usize {
        self.starts.len()
    }

    /// Source insertion point for caret position `c` (between chars c-1 and
    /// c). After a char we insert right after it, so typing continues that
    /// char's formatting (like a word processor).
    fn ins(&self, c: usize) -> usize {
        let n = self.n();
        if n == 0 {
            self.empty_ins
        } else if c == 0 {
            self.starts[0]
        } else {
            self.ends[c.min(n) - 1]
        }
    }

    fn caret_rect(&self, c: usize) -> Rect {
        self.galley.pos_from_cursor(CCursor::new(c)).translate(self.pos.to_vec2())
    }

    fn chars(&self) -> Vec<char> {
        self.galley.text().chars().collect()
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn prev_word(chars: &[char], mut c: usize) -> usize {
    while c > 0 && !is_word(chars[c - 1]) {
        c -= 1;
    }
    while c > 0 && is_word(chars[c - 1]) {
        c -= 1;
    }
    c
}

fn next_word(chars: &[char], mut c: usize) -> usize {
    let n = chars.len();
    while c < n && !is_word(chars[c]) {
        c += 1;
    }
    while c < n && is_word(chars[c]) {
        c += 1;
    }
    c
}

fn word_at(chars: &[char], c: usize) -> (usize, usize) {
    let n = chars.len();
    if n == 0 {
        return (0, 0);
    }
    let c = c.min(n - 1);
    let class = |ch: char| if is_word(ch) { 0 } else if ch.is_whitespace() { 1 } else { 2 };
    let k = class(chars[c]);
    let (mut a, mut b) = (c, c + 1);
    while a > 0 && class(chars[a - 1]) == k {
        a -= 1;
    }
    while b < n && class(chars[b]) == k {
        b += 1;
    }
    (a, b)
}

fn line_start(text: &str, p: usize) -> usize {
    text[..p.min(text.len())].rfind('\n').map_or(0, |i| i + 1)
}

fn line_end(text: &str, p: usize) -> usize {
    let p = p.min(text.len());
    text[p..].find('\n').map_or(text.len(), |i| p + i)
}

/// Length of the block prefix (`## `, `- `, `1. `, `- [ ] `, `> `) on a line.
fn prefix_len(line: &str) -> usize {
    line.len() - strip_block_prefix(line).len()
}

// ---------------------------------------------------------------------------
// The editor
// ---------------------------------------------------------------------------

/// Something a widget inside a rendered block was clicked for.
enum Click {
    Task(usize),
    SelectImage(usize),
    ResizeImage(ImageRef, f32),
}

#[derive(Default)]
struct ImgUi {
    selected: Option<usize>,
    drag: Option<(usize, f32)>,
}

pub struct LiveEditor {
    blocks: Vec<Block>,
    rev: Option<u64>,
    /// The editor has keyboard focus (caret visible).
    pub editing: bool,
    /// Request keyboard focus on the next frame.
    pub focus_next: bool,
    rows: Vec<VRow>,
    content: Rect,
    desired_x: Option<f32>,
    /// Inline format to apply to the next typed text (Ctrl+B with no selection).
    /// (open, close) markup for the next typed text, e.g. ("**", "**").
    pending: Option<(String, String)>,
    img: ImgUi,
    blink_start: f64,
    scroll_caret: bool,
    queued: Vec<Event>,
    /// Command picked in the floating toolbar, for the app to apply.
    pub format: Option<Format>,
    /// Toolbar + open dropdown (for hover checks).
    bubble_rect: Rect,
    /// The toolbar bar alone (for placement).
    bubble_bar: Vec2,
    bubble_menu: BubbleMenu,
}

impl Default for LiveEditor {
    fn default() -> Self {
        Self {
            blocks: vec![],
            rev: None,
            editing: false,
            focus_next: false,
            rows: vec![],
            content: Rect::NOTHING,
            desired_x: None,
            pending: None,
            img: ImgUi::default(),
            blink_start: 0.0,
            scroll_caret: false,
            queued: vec![],
            format: None,
            bubble_rect: Rect::NOTHING,
            bubble_bar: Vec2::new(300.0, 40.0),
            bubble_menu: BubbleMenu::Closed,
        }
    }
}

impl LiveEditor {
    /// Toggle an inline format for the next typed text (no selection).
    pub fn set_pending(&mut self, mark: &'static str) {
        let p = (mark.to_owned(), mark.to_owned());
        self.pending = if self.pending.as_ref() == Some(&p) { None } else { Some(p) };
    }

    /// Visible text of each laid-out row (for tests).
    #[cfg(test)]
    pub fn row_texts(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.galley.text().to_owned()).collect()
    }

    /// Forget any pending format for the next typed text.
    pub fn clear_pending(&mut self) {
        self.pending = None;
    }

    /// Color the next typed text (`None` cancels).
    pub fn set_pending_color(&mut self, hex: Option<&str>) {
        self.pending = hex.map(|h| (markdown::color_open_tag(h), "</span>".to_owned()));
    }

    fn sync(&mut self, doc: &Doc) {
        if self.rev != Some(doc.rev) {
            self.blocks = markdown::parse_blocks(&doc.text);
            self.rev = Some(doc.rev);
        }
    }

    /// If `head` is between blocks, the index of the block after it.
    fn gap_index(&self, head: usize) -> Option<usize> {
        if self.blocks.iter().any(|b| b.range.start <= head && head <= b.range.end) {
            return None;
        }
        Some(self.blocks.iter().position(|b| b.range.start > head).unwrap_or(self.blocks.len()))
    }

    fn block_at(&self, p: usize) -> Option<&Block> {
        self.blocks.iter().find(|b| b.range.start <= p && p <= b.range.end)
    }

    /// Visual position (row, char) of source offset `p`.
    fn locate(&self, p: usize) -> Option<(usize, usize)> {
        for (i, r) in self.rows.iter().enumerate() {
            for c in 0..=r.n() {
                if r.ins(c) == p {
                    return Some((i, c));
                }
            }
        }
        for (i, r) in self.rows.iter().enumerate() {
            if let Some(c) = r.starts.iter().position(|&s| s == p) {
                return Some((i, c));
            }
        }
        for (i, r) in self.rows.iter().enumerate() {
            if r.ins(0) <= p && p <= r.ins(r.n()) {
                return Some((i, r.ends.iter().filter(|&&e| e <= p).count().min(r.n())));
            }
        }
        let last = self.rows.iter().enumerate().rev().find(|(_, r)| r.ins(r.n()) <= p);
        last.map(|(i, r)| (i, r.n())).or((!self.rows.is_empty()).then_some((0, 0)))
    }

    /// Row/char under a screen position. `dir`: restrict to rows above (-1)
    /// or below (+1) `from` (used for Up/Down across gaps).
    fn hit(&self, p: Pos2, dir: i8, from: Rect) -> Option<(usize, usize)> {
        let mut best: Option<(usize, f32)> = None;
        for (i, r) in self.rows.iter().enumerate() {
            if (dir < 0 && r.rect.top() >= from.top() - 1.0) || (dir > 0 && r.rect.bottom() <= from.bottom() + 1.0) {
                continue;
            }
            let dy = (r.rect.top() - p.y).max(p.y - r.rect.bottom()).max(0.0);
            let dx = (r.rect.left() - p.x).max(p.x - r.rect.right()).max(0.0);
            let d = dy * 1000.0 + dx;
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        let (i, _) = best?;
        let r = &self.rows[i];
        Some((i, r.galley.cursor_from_pos(p - r.pos).index.0.min(r.n())))
    }

    fn set_caret(&mut self, doc: &mut Doc, p: usize, extend: bool) {
        if extend {
            doc.sel.head = p;
        } else {
            doc.sel = Sel::caret(p);
        }
        self.pending = None;
        self.scroll_caret = true;
    }

    // -- Edits ---------------------------------------------------------------

    /// Delete source ranges (keeping markup balanced); caret goes to `caret`
    /// (a pre-edit offset, shifted).
    fn delete(&mut self, doc: &mut Doc, dels: Vec<Range<usize>>, caret: usize, typing: bool) {
        let cs = ops::containers(&doc.text);
        let dels = ops::expand_deletions(&cs, dels);
        if dels.is_empty() {
            return;
        }
        let caret = ops::shift(caret, &dels);
        let (span, repl) = ops::apply_deletions(&doc.text, &dels);
        doc.replace(span, &repl, Sel::caret(caret), typing);
    }

    fn delete_selection(&mut self, doc: &mut Doc) -> bool {
        if doc.sel.is_empty() {
            return false;
        }
        let r = doc.sel.range();
        self.delete(doc, vec![r.clone()], r.start, false);
        true
    }

    fn insert_text(&mut self, doc: &mut Doc, t: &str, typing: bool) {
        self.delete_selection(doc);
        let p = doc.sel.head;
        let cs = ops::containers(&doc.text);
        let (_, _, in_code_span) = ops::enclosing(&doc.text, &cs, p);
        let kind = self.block_at(p).map(|b| b.kind);
        let raw = in_code_span || matches!(kind, Some(BlockKind::Code | BlockKind::Html));
        let mut s = if raw { t.to_owned() } else { ops::escape_inline(t, kind == Some(BlockKind::Table)) };
        let mut caret = p + s.len();
        if let Some((open, close)) = self.pending.take()
            && !raw
        {
            caret = p + open.len() + s.len();
            s = format!("{open}{s}{close}");
        }
        doc.replace(p..p, &s, Sel::caret(caret), typing);
        self.scroll_caret = true;
    }

    fn paste(&mut self, doc: &mut Doc, text: &str) {
        let text = text.replace("\r\n", "\n");
        let kind = self.block_at(doc.sel.head).map(|b| b.kind);
        if matches!(kind, Some(BlockKind::Code | BlockKind::Html)) || !text.contains('\n') {
            return self.insert_text(doc, &text, false);
        }
        // Each pasted line becomes a paragraph.
        self.delete_selection(doc);
        let p = doc.sel.head;
        let body: Vec<String> = text.lines().map(|l| ops::escape_inline(l, false)).collect();
        let s = body.join("\n\n");
        doc.replace(p..p, &s, Sel::caret(p + s.len()), false);
    }

    fn selected_text(&self, doc: &Doc) -> String {
        let r = doc.sel.range();
        let mut out = Vec::new();
        for row in &self.rows {
            let chars = row.chars();
            let s: String = (0..row.n())
                .filter(|&k| r.start <= row.starts[k] && row.ends[k] <= r.end && row.starts[k] < r.end)
                .map(|k| chars[k])
                .collect();
            if !s.is_empty() {
                out.push(s);
            }
        }
        if out.is_empty() { doc.text[r].to_owned() } else { out.join("\n") }
    }

    fn backspace(&mut self, doc: &mut Doc, word: bool) {
        if self.delete_selection(doc) {
            return;
        }
        let Some((r, c)) = self.locate(doc.sel.head) else { return };
        let row = &self.rows[r];
        if c > 0 {
            let from = if word { prev_word(&row.chars(), c) } else { c - 1 };
            let dels = (from..c).map(|k| row.starts[k]..row.ends[k]).collect();
            let caret = row.ins(from);
            return self.delete(doc, dels, caret, true);
        }
        match row.kind {
            RowK::Code | RowK::Cell => {}
            RowK::Phantom => {
                let from = row.block.checked_sub(1).map_or(0, |i| self.blocks[i].range.end);
                let head = doc.sel.head;
                doc.replace(from..head, "", Sel::caret(from), false);
            }
            RowK::Flow => {
                let text = &doc.text;
                let ls = line_start(text, row.ins(0));
                let line = &text[ls..line_end(text, ls)];
                let pl = prefix_len(line);
                if pl > 0 {
                    // Remove the list bullet / heading / quote marker first.
                    let prev_line_blank = ls < 2 || text[..ls - 1].ends_with('\n');
                    let repl = if prev_line_blank { "" } else { "\n" };
                    let caret = row.ins(0) - pl + repl.len();
                    return doc.replace(ls..ls + pl, repl, Sel::caret(caret), false);
                }
                self.merge_up(doc, r);
            }
        }
    }

    fn delete_forward(&mut self, doc: &mut Doc, word: bool) {
        if self.delete_selection(doc) {
            return;
        }
        let Some((r, c)) = self.locate(doc.sel.head) else { return };
        let row = &self.rows[r];
        if c < row.n() {
            let to = if word { next_word(&row.chars(), c + 1) } else { c + 1 };
            let dels = (c..to).map(|k| row.starts[k]..row.ends[k]).collect();
            let caret = row.ins(c);
            return self.delete(doc, dels, caret, true);
        }
        match row.kind {
            RowK::Code | RowK::Cell => {}
            RowK::Phantom => {
                let to = self.blocks.get(row.block).map_or(doc.text.len(), |b| b.range.start);
                let head = doc.sel.head;
                doc.replace(head..to, "", Sel::caret(head), false);
            }
            RowK::Flow => {
                if r + 1 < self.rows.len() {
                    self.merge_up(doc, r + 1);
                }
            }
        }
    }

    /// Join row `r` onto the end of the previous text row (or remove a
    /// non-text block, like a divider or image, sitting between them).
    fn merge_up(&mut self, doc: &mut Doc, r: usize) {
        if r == 0 {
            return;
        }
        let (prev, cur) = (&self.rows[r - 1], &self.rows[r]);
        if !matches!(prev.kind, RowK::Flow | RowK::Phantom) || cur.kind != RowK::Flow {
            return;
        }
        let head = doc.sel.head;
        // A block without text (divider, image) in between: delete it instead.
        // (Blocks strictly between two consecutive text rows have no text.)
        if prev.kind != RowK::Phantom && cur.block > prev.block + 1 {
            let bi = cur.block - 1;
            let from = self.blocks[bi - 1].range.end;
            let to = self.blocks[bi].range.end;
            let caret = if head >= to { head - (to - from) } else { head };
            return doc.replace(from..to, "", Sel::caret(caret), false);
        }
        let text = &doc.text;
        let a = line_end(text, prev.ins(prev.n()));
        let ls = line_start(text, cur.ins(0));
        let b = ls + prefix_len(&text[ls..line_end(text, ls)]);
        if a < b {
            doc.replace(a..b, "", Sel::caret(a), false);
        }
    }

    fn enter(&mut self, doc: &mut Doc, shift: bool) {
        self.delete_selection(doc);
        let p = doc.sel.head;
        let Some((r, c)) = self.locate(p) else { return };
        let row = &self.rows[r];
        let kind = self.block_at(p).map_or(BlockKind::Paragraph, |b| b.kind);
        let (n, row_kind) = (row.n(), row.kind);
        let text = doc.text.clone();
        let ls = line_start(&text, p);
        let le = line_end(&text, p);
        let line = &text[ls..le];
        let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let trimmed = &line[indent.len()..];
        let cs = ops::containers(&text);
        let (close, open, _) = ops::enclosing(&text, &cs, p);

        let mut put = |at: Range<usize>, s: String, caret: usize| doc.replace(at, &s, Sel::caret(caret), false);

        match row_kind {
            RowK::Cell => return,
            RowK::Phantom if !shift => return,
            RowK::Code => return put(p..p, format!("\n{indent}"), p + 1 + indent.len()),
            _ => {}
        }
        if shift {
            let s = "\\\n";
            return put(p..p, s.into(), p + s.len());
        }
        let at_end = c >= n;
        let at_start = c == 0 && n > 0;
        match kind {
            BlockKind::List => {
                let (marker_len, next) = if let Some(m) = ordered_prefix_len(trimmed) {
                    let num: u64 = trimmed[..m - 2].parse().unwrap_or(1);
                    (m, format!("{}{} ", num + 1, &trimmed[m - 2..m - 1]))
                } else if ["- ", "* ", "+ "].iter().any(|b| trimmed.starts_with(b)) {
                    (2, trimmed[..2].to_owned())
                } else {
                    return put(p..p, format!("\n{indent}  "), p + 3 + indent.len());
                };
                let mut next = next;
                let mut rest = &trimmed[marker_len..];
                for t in ["[ ] ", "[x] ", "[X] "] {
                    if let Some(r) = rest.strip_prefix(t) {
                        rest = r;
                        next.push_str("[ ] ");
                    }
                }
                if rest.trim().is_empty() {
                    // Enter on an empty item ends the list.
                    put(ls..le, "\n".into(), ls + 1);
                } else if at_end {
                    let s = format!("\n{indent}{next}");
                    put(le..le, s.clone(), le + s.len());
                } else if at_start {
                    let s = format!("{indent}{}\n", &trimmed[..marker_len]);
                    put(ls..ls, s, ls + indent.len() + marker_len);
                } else {
                    let s = format!("{close}\n{indent}{next}{open}");
                    let caret = p + s.len();
                    put(p..p, s, caret);
                }
            }
            BlockKind::Quote => {
                let pl = trimmed.len() - trimmed.trim_start_matches(['>', ' ']).len();
                let prefix = format!("{indent}{}", &trimmed[..pl]);
                if trimmed[pl..].trim().is_empty() {
                    put(ls..le, "\n".into(), ls + 1);
                } else if at_end {
                    let s = format!("\n{}\n{prefix}", prefix.trim_end());
                    put(le..le, s.clone(), le + s.len());
                } else {
                    let s = format!("{close}\n{}\n{prefix}{open}", prefix.trim_end());
                    let caret = p + s.len();
                    put(p..p, s, caret);
                }
            }
            BlockKind::Table | BlockKind::Code => {}
            _ => {
                if at_end || n == 0 {
                    put(le..le, "\n\n".into(), le + 2);
                } else if at_start {
                    // New empty line above: the caret moves to it.
                    put(ls..ls, "\n\n".into(), ls);
                } else {
                    let s = format!("{close}\n\n{open}");
                    let caret = p + s.len();
                    put(p..p, s, caret);
                }
            }
        }
    }

    // -- Input ---------------------------------------------------------------

    /// Returns `true` if the document text changed.
    fn apply_event(&mut self, ctx: &egui::Context, doc: &mut Doc, ev: Event) -> bool {
        let rev = doc.rev;
        let blink = ctx.input(|i| i.time);
        match ev {
            Event::Text(t) => self.insert_text(doc, &t, true),
            Event::Ime(egui::ImeEvent::Commit(t)) => self.insert_text(doc, &t, true),
            Event::Paste(t) => self.paste(doc, &t),
            Event::Copy => ctx.copy_text(self.selected_text(doc)),
            Event::Cut => {
                ctx.copy_text(self.selected_text(doc));
                self.delete_selection(doc);
            }
            Event::Key { key, pressed: true, modifiers: m, .. } => self.key(ctx, doc, key, m),
            _ => {}
        }
        self.blink_start = blink;
        doc.rev != rev
    }

    fn key(&mut self, ctx: &egui::Context, doc: &mut Doc, key: Key, m: Modifiers) {
        let shift = m.shift;
        let word = m.command || m.alt;
        let loc = self.locate(doc.sel.head);
        let collapse = !shift && !doc.sel.is_empty();
        let vertical = matches!(key, Key::ArrowUp | Key::ArrowDown | Key::PageUp | Key::PageDown);
        if !vertical {
            self.desired_x = None;
        }
        match key {
            Key::A if m.command => {
                doc.sel = Sel { anchor: 0, head: doc.text.len() };
            }
            Key::ArrowLeft | Key::ArrowRight => {
                let left = key == Key::ArrowLeft;
                if collapse {
                    let r = doc.sel.range();
                    return self.set_caret(doc, if left { r.start } else { r.end }, false);
                }
                let Some((r, c)) = loc else { return };
                let row = &self.rows[r];
                let target = if left {
                    if c > 0 {
                        Some((r, if word { prev_word(&row.chars(), c) } else { c - 1 }))
                    } else {
                        r.checked_sub(1).map(|p| (p, self.rows[p].n()))
                    }
                } else if c < row.n() {
                    Some((r, if word { next_word(&row.chars(), c) } else { c + 1 }))
                } else {
                    (r + 1 < self.rows.len()).then_some((r + 1, 0))
                };
                if let Some((r, c)) = target {
                    let p = self.rows[r].ins(c);
                    self.set_caret(doc, p, shift);
                }
            }
            Key::ArrowUp | Key::ArrowDown | Key::PageUp | Key::PageDown => {
                let Some((r, c)) = loc else { return };
                let up = matches!(key, Key::ArrowUp | Key::PageUp);
                let rect = self.rows[r].caret_rect(c);
                let x = *self.desired_x.get_or_insert(rect.center().x);
                let step = if matches!(key, Key::PageUp | Key::PageDown) { ctx.content_rect().height() * 0.85 } else { 3.0 };
                let y = if up { rect.top() - step } else { rect.bottom() + step };
                let target = self.hit(Pos2::new(x, y), if up { -1 } else { 1 }, rect).or_else(|| {
                    if up { Some((0, 0)) } else { self.rows.len().checked_sub(1).map(|l| (l, self.rows[l].n())) }
                });
                if let Some((r, c)) = target {
                    let p = self.rows[r].ins(c);
                    let keep = self.desired_x;
                    self.set_caret(doc, p, shift);
                    self.desired_x = keep;
                }
            }
            Key::Home | Key::End => {
                let home = key == Key::Home;
                let target = if m.command {
                    if home { Some((0, 0)) } else { self.rows.len().checked_sub(1).map(|l| (l, self.rows[l].n())) }
                } else {
                    loc.map(|(r, c)| {
                        let g = &self.rows[r].galley;
                        let cc = CCursor::new(c);
                        let t = if home { g.cursor_begin_of_row(&cc) } else { g.cursor_end_of_row(&cc) };
                        (r, t.index.0.min(self.rows[r].n()))
                    })
                };
                if let Some((r, c)) = target {
                    let p = self.rows[r].ins(c);
                    self.set_caret(doc, p, shift);
                }
            }
            Key::Backspace => self.backspace(doc, word),
            Key::Delete => self.delete_forward(doc, word),
            Key::Enter => self.enter(doc, shift),
            Key::Tab => {
                if loc.is_some_and(|(r, _)| self.rows[r].kind == RowK::Code) {
                    self.insert_text(doc, "    ", true);
                }
            }
            Key::Escape => {
                ctx.memory_mut(|mem| mem.surrender_focus(Id::new(LIVE_ID)));
                self.pending = None;
            }
            _ => {}
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context, doc: &mut Doc) {
        let ours = |e: &Event| match e {
            Event::Text(_) | Event::Paste(_) | Event::Copy | Event::Cut => true,
            Event::Ime(egui::ImeEvent::Commit(_)) => true,
            Event::Key { key, pressed: true, modifiers, .. } => {
                matches!(
                    key,
                    Key::ArrowLeft
                        | Key::ArrowRight
                        | Key::ArrowUp
                        | Key::ArrowDown
                        | Key::Home
                        | Key::End
                        | Key::PageUp
                        | Key::PageDown
                        | Key::Backspace
                        | Key::Delete
                        | Key::Enter
                        | Key::Tab
                        | Key::Escape
                ) || (*key == Key::A && modifiers.command)
            }
            _ => false,
        };
        let mut events = std::mem::take(&mut self.queued);
        events.extend(ctx.input_mut(|i| {
            let mine: Vec<Event> = i.events.iter().filter(|e| ours(e)).cloned().collect();
            i.events.retain(|e| !ours(e));
            mine
        }));
        let mut it = events.into_iter();
        while let Some(ev) = it.next() {
            if self.apply_event(ctx, doc, ev) {
                // Positions are stale until the next layout; finish next frame.
                self.queued = it.collect();
                if !self.queued.is_empty() {
                    ctx.request_repaint();
                }
                break;
            }
        }
    }

    fn handle_image_keys(&mut self, ctx: &egui::Context, doc: &mut Doc) {
        let Some(start) = self.img.selected else { return };
        let pressed = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
        if pressed(Key::Escape) {
            self.img.selected = None;
            return;
        }
        let img = self
            .blocks
            .iter()
            .flat_map(|b| &b.rows)
            .flat_map(|r| &r.spans)
            .filter_map(|s| s.image.as_deref())
            .find(|i| i.range.start == start)
            .cloned();
        if let Some(img) = img
            && (pressed(Key::Delete) || pressed(Key::Backspace))
        {
            doc.replace(img.range.clone(), "", Sel::caret(img.range.start), false);
            self.img.selected = None;
        }
    }

    // -- Frame ---------------------------------------------------------------

    pub fn ui(&mut self, ui: &mut Ui, doc: &mut Doc, theme: &Theme, view: &View) {
        let ctx = ui.ctx().clone();
        let id = Id::new(LIVE_ID);
        self.sync(doc);

        // One widget behind the whole document handles pointer + keyboard focus.
        let bg = ui.interact(self.content, id, Sense::click_and_drag());
        if std::mem::take(&mut self.focus_next) {
            ctx.memory_mut(|m| m.request_focus(id));
            ctx.request_repaint(); // the focus-lock filter applies from next frame
            self.blink_start = ctx.input(|i| i.time);
        }
        let focused = ctx.memory(|m| m.has_focus(id));
        self.editing = focused;
        if focused {
            let filter = EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: false };
            ctx.memory_mut(|m| m.set_focus_lock_filter(id, filter));
            self.img.selected = None;
            self.handle_keys(&ctx, doc);
            self.sync(doc);
        } else {
            self.handle_image_keys(&ctx, doc);
            self.sync(doc);
        }

        // Render.
        let width = ui.available_width();
        let origin = ui.cursor().min;
        let head = doc.sel.head;
        let gap = if focused { self.gap_index(head) } else { None };
        let sel = doc.sel.range();
        let mut rows: Vec<VRow> = Vec::new();
        let mut click = None;
        ui.spacing_mut().item_spacing.y = 0.0;
        let n = self.blocks.len();
        for i in 0..=n {
            if gap == Some(i) {
                if i > 0 {
                    ui.add_space(block_gap(BlockKind::Paragraph));
                }
                let laid = lay_out(ui, &[], theme, 0, false, width, false, false);
                let h = laid.galley.size().y.max(BASE * 1.6);
                let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
                rows.push(VRow {
                    galley: laid.galley,
                    pos: rect.min,
                    rect,
                    starts: vec![],
                    ends: vec![],
                    empty_ins: head,
                    kind: RowK::Phantom,
                    block: i,
                    links: vec![],
                });
            }
            if i == n {
                break;
            }
            let block = &self.blocks[i];
            if i > 0 || gap == Some(0) {
                ui.add_space(block_gap(block.kind));
            }
            let top = ui.cursor().top();
            let mut rc = Rc {
                theme,
                marks: marks_in(view.marks, block.range.clone()),
                sel: sel.clone(),
                base_dir: view.base_dir,
                img: &mut self.img,
                rows: &mut rows,
                block: i,
                src: &doc.text,
            };
            if let Some(c) = render_block(ui, block, &mut rc, width) {
                click = Some(c);
            }
            if let Some(b) = view.scroll_to
                && block.range.contains(&b)
            {
                let r = Rect::from_min_max(Pos2::new(origin.x, top), Pos2::new(origin.x + width, ui.cursor().top()));
                ui.scroll_to_rect(r.expand(40.0), Some(egui::Align::Center));
            }
        }
        // Empty space below the document is part of the editor too.
        let rest = ui.available_size_before_wrap().max(Vec2::new(width, 120.0));
        ui.allocate_exact_size(rest, Sense::hover());
        self.content = Rect::from_min_max(origin, Pos2::new(origin.x + width, ui.cursor().top()));
        self.rows = rows;

        self.handle_widget_click(&ctx, doc, click);
        self.handle_pointer(ui, &bg, doc);

        self.bubble(ui, doc, theme, focused);

        // Caret.
        if focused && let Some((r, c)) = self.locate(doc.sel.head) {
            let rect = self.rows[r].caret_rect(c);
            if std::mem::take(&mut self.scroll_caret) {
                ui.scroll_to_rect(rect.expand2(Vec2::new(0.0, 30.0)), None);
            }
            let t = ctx.input(|i| i.time) - self.blink_start;
            if ((t / BLINK) as i64) % 2 == 0 {
                let w = if self.pending.is_some() { 3.0 } else { 2.0 };
                ui.painter().vline(rect.center().x, rect.y_range(), Stroke::new(w, theme.accent));
            }
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(BLINK - t.rem_euclid(BLINK) + 0.01));
            ctx.output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    purpose: Default::default(),
                    rect: ui.clip_rect(),
                    cursor_rect: rect,
                    should_interrupt_composition: false,
                })
            });
        }
    }

    /// Floating formatting toolbar over a non-empty selection.
    fn bubble(&mut self, ui: &Ui, doc: &Doc, theme: &Theme, focused: bool) {
        let ctx = ui.ctx();
        let (pointer, down) = ctx.input(|i| (i.pointer.hover_pos(), i.pointer.primary_down()));
        let over = pointer.is_some_and(|p| self.bubble_rect.expand(6.0).contains(p));
        let r = doc.sel.range();
        // Keep it while the pointer is on it: clicking a button takes focus
        // from the editor, but the click must still land.
        let menu_open = self.bubble_menu != BubbleMenu::Closed;
        let show = !r.is_empty() && (focused || over || menu_open) && !(down && !over && !menu_open);
        if !show {
            if !down {
                self.bubble_menu = BubbleMenu::Closed;
            }
            if r.is_empty() || !focused {
                self.bubble_rect = Rect::NOTHING;
            }
            return;
        }
        let (Some((ra, ca)), Some((rb, cb))) = (self.locate(r.start), self.locate(r.end)) else { return };
        let (a, b) = (self.rows[ra].caret_rect(ca), self.rows[rb].caret_rect(cb));
        let clip = ui.clip_rect();
        let size = self.bubble_bar;
        let mut pos = Pos2::new(a.left().min(b.left()), a.top() - size.y - 8.0);
        if pos.y < clip.top() + 4.0 {
            pos.y = b.bottom() + 8.0; // no room above: show below
        }
        pos.x = pos.x.clamp(clip.left() + 4.0, (clip.right() - size.x - 4.0).max(clip.left() + 4.0));

        // What's already applied to the selection (to show buttons as active).
        let cs = ops::containers(&doc.text);
        let active = |marks: &[&str]| {
            cs.iter().any(|c| marks.contains(&&doc.text[c.open.clone()]) && c.open.end <= r.start && r.end <= c.close.start)
        };
        let current_color = selection_color(&doc.text, &cs, r.clone());
        let block = self.block_at(r.start).map(|b| b.kind);
        let line = &doc.text[line_start(&doc.text, r.start)..line_end(&doc.text, r.start)];
        let lt = line.trim_start();
        let label = match block {
            Some(BlockKind::Heading(n)) => format!("Heading {n}"),
            Some(BlockKind::Quote) => "Quote".into(),
            Some(BlockKind::List) if lt.starts_with("- [") => "Task list".into(),
            Some(BlockKind::List) if ordered_prefix_len(lt).is_some() => "Numbered".into(),
            Some(BlockKind::List) => "Bullet list".into(),
            _ => "Text".into(),
        };

        let mut pick = None;
        let frame = egui::Frame::popup(ui.style())
            .fill(theme.panel)
            .stroke(Stroke::new(1.0, theme.border))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(6, 4));
        let resp = egui::Area::new(Id::new("mdv-bubble"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let menu_btn = egui::Button::new(
                            egui::RichText::new(format!("{label}  {}", egui_phosphor::regular::CARET_DOWN)).size(13.5),
                        )
                        .frame_when_inactive(false)
                        .selected(self.bubble_menu == BubbleMenu::Block)
                        .min_size(Vec2::new(0.0, 28.0));
                        let mb = ui.add(menu_btn);
                        if mb.clicked() {
                            self.bubble_menu =
                                if self.bubble_menu == BubbleMenu::Block { BubbleMenu::Closed } else { BubbleMenu::Block };
                        }
                        ui.add_space(4.0);
                        let (sr, _) = ui.allocate_exact_size(Vec2::new(1.0, 18.0), Sense::hover());
                        ui.painter().rect_filled(sr, 0.0, theme.border);
                        ui.add_space(4.0);
                        use egui_phosphor::regular as icon;
                        for (glyph, tip, f, on) in [
                            (icon::TEXT_B, "Bold (Ctrl+B)", Format::Wrap("**"), active(&["**", "__"])),
                            (icon::TEXT_ITALIC, "Italic (Ctrl+I)", Format::Wrap("*"), active(&["*", "_"])),
                            (icon::TEXT_STRIKETHROUGH, "Strikethrough", Format::Wrap("~~"), active(&["~~", "~"])),
                            (icon::CODE, "Inline code (Ctrl+E)", Format::Wrap("`"), active(&["`"])),
                            (icon::LINK, "Link (Ctrl+K)", Format::Link, active(&["["])),
                        ] {
                            let b = egui::Button::new(egui::RichText::new(glyph).size(16.0))
                                .frame_when_inactive(on)
                                .selected(on)
                                .min_size(Vec2::splat(28.0));
                            if ui.add(b).on_hover_text(tip).clicked() {
                                pick = Some(f);
                            }
                        }
                        let open = self.bubble_menu == BubbleMenu::Color;
                        if color_button(ui, theme, current_color, open).on_hover_text("Text color").clicked() {
                            self.bubble_menu = if open { BubbleMenu::Closed } else { BubbleMenu::Color };
                        }
                        let clear = egui::Button::new(egui::RichText::new(icon::TEXT_T_SLASH).size(16.0))
                            .frame_when_inactive(false)
                            .min_size(Vec2::splat(28.0));
                        if ui.add(clear).on_hover_text("Clear formatting (Ctrl+\\)").clicked() {
                            pick = Some(Format::Clear);
                        }
                    });
                });
            });
        let mut rect = resp.response.rect;
        self.bubble_bar = rect.size();

        if self.bubble_menu == BubbleMenu::Color {
            let mresp = egui::Area::new(Id::new("mdv-bubble-colors"))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.left_bottom() + Vec2::new(0.0, 4.0))
                .show(ctx, |ui| {
                    frame.show(ui, |ui| {
                        if let Some(c) = color_picker(ui, theme, current_color) {
                            pick = Some(Format::Color(c));
                        }
                    })
                });
            rect = rect.union(mresp.response.rect);
        }
        if self.bubble_menu == BubbleMenu::Block {
            let mresp = egui::Area::new(Id::new("mdv-bubble-menu"))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.left_bottom() + Vec2::new(0.0, 4.0))
                .show(ctx, |ui| {
                    frame.show(ui, |ui| {
                        ui.set_min_width(160.0);
                        use egui_phosphor::regular as icon;
                        for (glyph, name, f) in [
                            (icon::TEXT_T, "Text", Format::Heading(0)),
                            (icon::TEXT_H_ONE, "Heading 1", Format::Heading(1)),
                            (icon::TEXT_H_TWO, "Heading 2", Format::Heading(2)),
                            (icon::TEXT_H_THREE, "Heading 3", Format::Heading(3)),
                            (icon::TEXT_H_FOUR, "Heading 4", Format::Heading(4)),
                            (icon::TEXT_H_FIVE, "Heading 5", Format::Heading(5)),
                            (icon::TEXT_H_SIX, "Heading 6", Format::Heading(6)),
                            (icon::LIST_BULLETS, "Bullet list", Format::Prefix(Prefix::Bullet)),
                            (icon::LIST_NUMBERS, "Numbered list", Format::Prefix(Prefix::Number)),
                            (icon::LIST_CHECKS, "Task list", Format::Prefix(Prefix::Task)),
                            (icon::QUOTES, "Quote", Format::Prefix(Prefix::Quote)),
                        ] {
                            let b = egui::Button::new(format!("{glyph}  {name}"))
                                .frame_when_inactive(false)
                                .selected(name == label)
                                .min_size(Vec2::new(150.0, 26.0));
                            if ui.add(b).clicked() {
                                pick = Some(f);
                            }
                        }
                    });
                });
            rect = rect.union(mresp.response.rect);
        }
        self.bubble_rect = rect;
        if let Some(f) = pick {
            self.bubble_menu = BubbleMenu::Closed;
            self.format = Some(f);
        }
    }

    fn handle_widget_click(&mut self, ctx: &egui::Context, doc: &mut Doc, click: Option<Click>) {
        match click {
            Some(Click::Task(at)) => doc.toggle_task(at),
            Some(Click::SelectImage(start)) => {
                self.img.selected = Some(start);
                ctx.memory_mut(|m| m.surrender_focus(Id::new(LIVE_ID)));
            }
            Some(Click::ResizeImage(img, w)) => {
                let md = images::to_markdown(&img.src, &img.alt, Some(Width::Px(w)));
                doc.replace(img.range.clone(), &md, Sel::caret(img.range.start), false);
                self.img.selected = Some(img.range.start);
            }
            None => {}
        }
    }

    fn handle_pointer(&mut self, ui: &Ui, bg: &egui::Response, doc: &mut Doc) {
        let ctx = ui.ctx();
        let (mods, pressed) = ui.input(|i| (i.modifiers, i.pointer.primary_pressed()));

        // The pointer is over something floating above the document (the
        // selection toolbar, a menu, a dialog): that's not a click on the text.
        // Without this, pressing a floating-toolbar button also moved the
        // caret/selection first, so e.g. Italic wrapped the wrong range.
        let my_layer = ui.layer_id();
        let over_other = |p: Pos2| self.bubble_rect.contains(p) || ctx.layer_id_at(p).is_some_and(|l| l != my_layer);
        if bg.interact_pointer_pos().or(bg.hover_pos()).is_some_and(over_other) && !bg.dragged() {
            return;
        }

        // Hover: text cursor over text, hand over links with Ctrl.
        if let Some(h) = bg.hover_pos()
            && let Some((r, c)) = self.hit(h, 0, Rect::NOTHING)
            && self.rows[r].rect.contains(h)
        {
            let on_link = self.rows[r].links.iter().any(|(lr, _)| lr.contains(&c) || (c > 0 && lr.contains(&(c - 1))));
            ctx.set_cursor_icon(if on_link && mods.command { CursorIcon::PointingHand } else { CursorIcon::Text });
        }

        let Some(p) = bg.interact_pointer_pos() else { return };
        let hit = self.hit(p, 0, Rect::NOTHING);
        if pressed && bg.is_pointer_button_down_on() {
            ctx.memory_mut(|m| m.request_focus(Id::new(LIVE_ID)));
            ctx.request_repaint();
            self.img.selected = None;
            self.desired_x = None;
            self.blink_start = ui.input(|i| i.time);
            match hit {
                Some((r, c)) => {
                    let link = self.rows[r].links.iter().find(|(lr, _)| lr.contains(&c)).map(|(_, u)| u.clone());
                    if mods.command
                        && let Some(url) = link
                    {
                        ctx.open_url(egui::OpenUrl::new_tab(url));
                        return;
                    }
                    let ins = self.rows[r].ins(c);
                    self.set_caret(doc, ins, mods.shift);
                }
                None => {
                    let end = doc.text.len();
                    self.set_caret(doc, end, mods.shift);
                }
            }
        } else if bg.dragged()
            && let Some((r, c)) = hit
        {
            doc.sel.head = self.rows[r].ins(c);
            // Auto-scroll when dragging past the edges.
            let clip = ui.clip_rect();
            if p.y < clip.top() + 16.0 {
                ui.scroll_with_delta(Vec2::new(0.0, 14.0));
            } else if p.y > clip.bottom() - 16.0 {
                ui.scroll_with_delta(Vec2::new(0.0, -14.0));
            }
        }

        if let Some((r, c)) = hit {
            let row = &self.rows[r];
            if bg.triple_clicked() {
                doc.sel = Sel { anchor: row.ins(0), head: row.ins(row.n()) };
            } else if bg.double_clicked() {
                let (a, b) = word_at(&row.chars(), c);
                doc.sel = Sel { anchor: row.ins(a), head: row.ins(b) };
            }
        }
    }
}

/// Color of the innermost color span around `r`, if any.
pub fn selection_color(text: &str, cs: &[ops::Container], r: Range<usize>) -> Option<[u8; 3]> {
    cs.iter()
        .filter(|c| c.html && c.open.end <= r.start && r.end <= c.close.start)
        .max_by_key(|c| c.open.start)
        .and_then(|c| markdown::span_open_color(&text[c.open.clone()]).flatten())
}

fn block_gap(kind: BlockKind) -> f32 {
    match kind {
        BlockKind::Heading(1 | 2) => 28.0,
        BlockKind::Heading(_) => 20.0,
        _ => 12.0,
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Everything the block renderers need besides the block itself.
struct Rc<'a> {
    theme: &'a Theme,
    marks: &'a Marks,
    sel: Range<usize>,
    base_dir: Option<&'a Path>,
    img: &'a mut ImgUi,
    rows: &'a mut Vec<VRow>,
    block: usize,
    src: &'a str,
}

struct Laid {
    galley: Arc<egui::Galley>,
    starts: Vec<usize>,
    ends: Vec<usize>,
    links: Vec<(Range<usize>, String)>,
}

/// Lay out spans. `center`: rows are centered around x = 0, so paint at
/// `left + wrap / 2`.
#[allow(clippy::too_many_arguments)]
fn lay_out(ui: &Ui, spans: &[Span], theme: &Theme, heading: u8, code: bool, wrap: f32, bold: bool, center: bool) -> Laid {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap;
    if center {
        job.halign = egui::Align::Center;
    }
    let (mut starts, mut ends, mut links) = (Vec::new(), Vec::new(), Vec::new());
    for s in spans.iter().filter(|s| s.image.is_none()) {
        let mut style = s.style.clone();
        style.bold |= bold;
        job.append(&s.text, 0.0, theme.rendered(&style, heading, code));
        let first = starts.len();
        for (bi, ch) in s.text.char_indices() {
            if s.exact {
                starts.push(s.src + bi);
                ends.push(s.src + bi + ch.len_utf8());
            } else {
                starts.push(s.src);
                ends.push(s.src_end);
            }
        }
        if let Some(url) = &s.style.link {
            links.push((first..starts.len(), url.clone()));
        }
    }
    if job.text.is_empty() {
        job.append("", 0.0, theme.rendered(&Default::default(), heading, code));
    }
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    Laid { galley, starts, ends, links }
}

/// Paint selection and find highlights behind the text, then the text.
fn paint_text(ui: &Ui, laid: &Laid, pos: Pos2, rc: &Rc) {
    let theme = rc.theme;
    let n = laid.starts.len();
    if n > 0 && (!rc.sel.is_empty() || !rc.marks.is_empty()) {
        let color_of = |k: usize| -> Option<Color32> {
            let (s, e) = (laid.starts[k], laid.ends[k]);
            if !rc.sel.is_empty() && rc.sel.start <= s && e <= rc.sel.end && s < rc.sel.end {
                return Some(theme.selection);
            }
            let i = rc.marks.partition_point(|(m, _)| m.end <= s);
            rc.marks
                .get(i)
                .filter(|(m, _)| m.start <= s)
                .map(|(_, cur)| if *cur { theme.find_current } else { theme.find })
        };
        let mut acc: Option<(Rect, Color32)> = None;
        let flush = |a: Option<(Rect, Color32)>| {
            if let Some((r, col)) = a {
                ui.painter().rect_filled(r.translate(pos.to_vec2()), 2.0, col);
            }
        };
        for k in 0..n {
            let Some(col) = color_of(k) else {
                flush(acc.take());
                continue;
            };
            let a = laid.galley.pos_from_cursor(CCursor::new(k));
            let b = laid.galley.pos_from_cursor(CCursor::new(k + 1));
            let r = if (a.min.y - b.min.y).abs() > 0.5 {
                // Last char on a wrapped row: extend a little.
                Rect::from_min_max(a.min, Pos2::new(a.min.x + 4.0, a.max.y))
            } else {
                Rect::from_min_max(a.min, Pos2::new(b.min.x, a.max.y))
            };
            match &mut acc {
                Some((ar, ac)) if *ac == col && (ar.max.x - r.min.x).abs() < 0.5 && (ar.min.y - r.min.y).abs() < 0.5 => {
                    *ar = ar.union(r);
                }
                _ => {
                    flush(acc.take());
                    acc = Some((r, col));
                }
            }
        }
        flush(acc);
    }
    ui.painter().galley(pos, laid.galley.clone(), theme.text);
}

fn push_row(rc: &mut Rc, laid: Laid, pos: Pos2, rect: Rect, kind: RowK, empty_ins: usize) {
    rc.rows.push(VRow {
        galley: laid.galley,
        pos,
        rect,
        starts: laid.starts,
        ends: laid.ends,
        empty_ins,
        kind,
        block: rc.block,
        links: laid.links,
    });
}

const LIST_INDENT: f32 = 26.0;
const QUOTE_INDENT: f32 = 20.0;

fn render_block(ui: &mut Ui, block: &Block, rc: &mut Rc, width: f32) -> Option<Click> {
    let theme = rc.theme;
    let mut click = None;
    if block.rows.is_empty() && block.kind != BlockKind::Rule {
        // Nothing visible (e.g. an empty quote): still give the caret a home.
        let laid = lay_out(ui, &[], theme, 0, false, width, false, false);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, laid.galley.size().y), Sense::hover());
        push_row(rc, laid, rect.min, rect, RowK::Flow, block.range.end);
        return None;
    }
    for (ri, row) in block.rows.iter().enumerate() {
        if ri > 0 {
            ui.add_space(match row.kind {
                RowKind::Text if row.marker.is_some() => 4.0,
                _ => 10.0,
            });
        }
        let c = match &row.kind {
            RowKind::Rule => {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
                ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, theme.border));
                None
            }
            RowKind::Code => {
                let pad = Vec2::new(16.0, 14.0);
                let laid = lay_out(ui, &row.spans, theme, 0, true, width - pad.x * 2.0, false, false);
                let size = Vec2::new(width, laid.galley.size().y.max(MONO * 1.4) + pad.y * 2.0);
                let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                ui.painter().rect_filled(rect, 8.0, theme.code_bg);
                ui.painter().rect_stroke(rect, 8.0, Stroke::new(1.0, theme.border), egui::StrokeKind::Inside);
                paint_text(ui, &laid, rect.min + pad, rc);
                // Empty block: the caret goes on the line after the opening fence.
                let empty_ins = (line_end(rc.src, block.range.start) + 1).min(block.range.end);
                push_row(rc, laid, rect.min + pad, rect, RowK::Code, empty_ins);
                None
            }
            RowKind::Table(trows) => render_table(ui, trows, rc, width),
            RowKind::Text => render_text_row(ui, row, rc, width),
        };
        if c.is_some() {
            click = c;
        }
    }
    click
}

fn render_text_row(ui: &mut Ui, row: &markdown::Row, rc: &mut Rc, width: f32) -> Option<Click> {
    let theme = rc.theme;
    let quote_w = row.quote_depth as f32 * QUOTE_INDENT;
    let list_w = row.list_depth as f32 * LIST_INDENT;
    let left = quote_w + list_w;

    // Rows with images: any text first, then the pictures.
    if row.spans.iter().any(|s| s.image.is_some()) {
        let imgs: Vec<ImageRef> = row.spans.iter().filter_map(|s| s.image.as_deref().cloned()).collect();
        let text: Vec<Span> = row.spans.iter().filter(|s| s.image.is_none()).cloned().collect();
        let mut click = None;
        if text.iter().any(|s| !s.text.trim().is_empty()) || row.marker.is_some() {
            let mut r = row.clone();
            r.spans = text;
            click = render_text_row(ui, &r, rc, width);
            ui.add_space(8.0);
        }
        let c = render_images(ui, &imgs, left, width, row.center, rc);
        return click.or(c);
    }

    let wrap = (width - left).max(50.0);
    let mut laid = lay_out(ui, &row.spans, theme, row.heading, false, wrap, false, row.center);
    if matches!(row.marker, Some(Marker::Task { checked: true, .. })) {
        // Dim completed tasks.
        let mut job = (*laid.galley.job).clone();
        for s in &mut job.sections {
            s.format.color = theme.faint;
            s.format.strikethrough = Stroke::new(1.0, theme.faint);
        }
        laid.galley = ui.fonts_mut(|f| f.layout_job(job));
    }
    let first_row_h = laid.galley.rows.first().map_or(BASE * 1.6, |r| r.height());
    let size = Vec2::new(width, laid.galley.size().y.max(first_row_h));
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();

    // Quote bars (extend up over the inter-row gap so they read as continuous).
    for q in 0..row.quote_depth {
        let x = rect.left() + q as f32 * QUOTE_INDENT + 2.0;
        painter.vline(x, (rect.top() - 6.0)..=rect.bottom(), Stroke::new(3.0, theme.quote_bar));
    }

    let mut click = None;
    if let Some(m) = &row.marker {
        let mx = rect.left() + quote_w + (row.list_depth.saturating_sub(1)) as f32 * LIST_INDENT;
        let cy = rect.top() + first_row_h / 2.0;
        match m {
            Marker::Bullet(depth) => {
                let r = 2.6;
                let c = Pos2::new(mx + 9.0, cy);
                match depth % 3 {
                    0 => painter.circle_filled(c, r, theme.text),
                    1 => painter.circle_stroke(c, r, Stroke::new(1.2, theme.text)),
                    _ => painter.rect_filled(Rect::from_center_size(c, Vec2::splat(r * 2.0)), 0.0, theme.text),
                };
            }
            Marker::Number(n) => {
                painter.text(
                    Pos2::new(mx + LIST_INDENT - 6.0, cy),
                    Align2::RIGHT_CENTER,
                    format!("{n}."),
                    FontId::proportional(BASE),
                    theme.faint,
                );
            }
            Marker::Task { checked, at } => {
                let b = Rect::from_center_size(Pos2::new(mx + 9.0, cy), Vec2::splat(15.0));
                let hit = ui.interact(b.expand(3.0), ui.id().with(("task", at)), Sense::click());
                if *checked {
                    painter.rect_filled(b, 4.0, theme.accent);
                    painter.text(b.center(), Align2::CENTER_CENTER, egui_phosphor::regular::CHECK, FontId::proportional(12.0), Color32::WHITE);
                } else {
                    let stroke = if hit.hovered() { theme.accent } else { theme.faint };
                    painter.rect_stroke(b, 4.0, Stroke::new(1.5, stroke), egui::StrokeKind::Inside);
                }
                if hit.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                if hit.clicked() {
                    click = Some(Click::Task(*at));
                }
            }
        }
    }

    let x = rect.left() + left + if row.center { wrap / 2.0 } else { 0.0 };
    let pos = Pos2::new(x, rect.top());
    paint_text(ui, &laid, pos, rc);
    // Empty list item / heading: the caret goes at the end of its line (after "- ").
    let empty_ins = line_end(rc.src, row.src);
    push_row(rc, laid, pos, rect, RowK::Flow, empty_ins);
    click
}

fn render_table(ui: &mut Ui, rows: &[markdown::TableRow], rc: &mut Rc, width: f32) -> Option<Click> {
    let theme = rc.theme;
    let cols = rows.iter().map(|r| r.cells.len()).max().unwrap_or(1).max(1);
    let pad = Vec2::new(12.0, 8.0);
    let col_w = width / cols as f32;
    let top = ui.cursor().top();
    let left = ui.cursor().left();

    for (ri, r) in rows.iter().enumerate() {
        let laid: Vec<Laid> = (0..cols)
            .map(|c| {
                let spans = r.cells.get(c).map_or(&[][..], |v| v.as_slice());
                lay_out(ui, spans, theme, 0, false, col_w - pad.x * 2.0, r.header, false)
            })
            .collect();
        let h = laid.iter().map(|l| l.galley.size().y).fold(BASE * 1.6, f32::max) + pad.y * 2.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
        if r.header {
            ui.painter().rect_filled(rect, CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 }, theme.code_bg);
        } else if ri % 2 == 0 {
            ui.painter().rect_filled(rect, 0.0, theme.panel);
        }
        for (c, l) in laid.into_iter().enumerate() {
            let cell = Rect::from_min_size(Pos2::new(rect.left() + c as f32 * col_w, rect.top()), Vec2::new(col_w, h));
            let pos = cell.min + pad;
            paint_text(ui, &l, pos, rc);
            if !l.starts.is_empty() {
                push_row(rc, l, pos, cell, RowK::Cell, 0);
            }
        }
        ui.painter().hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, theme.border));
    }
    let outer = Rect::from_min_max(Pos2::new(left, top), Pos2::new(left + width, ui.cursor().top()));
    ui.painter().rect_stroke(outer, 8.0, Stroke::new(1.0, theme.border), egui::StrokeKind::Inside);
    None
}

/// Draw a row of images (wrapping), with selection outline and resize handles.
fn render_images(ui: &mut Ui, imgs: &[ImageRef], left: f32, width: f32, center: bool, rc: &mut Rc) -> Option<Click> {
    use egui::load::{SizeHint, TexturePoll};
    let theme = rc.theme;
    let avail = (width - left).max(50.0);
    let ctx = ui.ctx().clone();
    let gap = 8.0;

    let items: Vec<(Result<TexturePoll, String>, Vec2)> = imgs
        .iter()
        .map(|img| {
            let uri = images::resolve_uri(&img.src, rc.base_dir);
            let poll = ctx.try_load_texture(&uri, egui::TextureOptions::LINEAR, SizeHint::default()).map_err(|e| e.to_string());
            let natural = match &poll {
                Ok(TexturePoll::Ready { texture }) => texture.size,
                Ok(TexturePoll::Pending { size }) => size.unwrap_or(Vec2::new(avail.min(320.0), 180.0)),
                Err(_) => Vec2::new(avail.min(320.0), 64.0),
            };
            let mut w = img.width.map_or(natural.x, |w| w.resolve(avail));
            if let Some((s, dw)) = rc.img.drag
                && s == img.range.start
            {
                w = dw;
            }
            let w = w.clamp(16.0, avail);
            let h = if poll.is_err() || natural.x <= 0.0 { natural.y } else { w * natural.y / natural.x };
            (poll, Vec2::new(w, h))
        })
        .collect();

    // Flow layout (optionally centering each line).
    let (mut x, mut y, mut line_h) = (0.0f32, 0.0f32, 0.0f32);
    let mut placed: Vec<Vec2> = Vec::with_capacity(items.len());
    let mut line_start = 0;
    let center_line = |placed: &mut [Vec2], line_w: f32| {
        if center {
            let shift = ((avail - line_w) / 2.0).max(0.0);
            placed.iter_mut().for_each(|p| p.x += shift);
        }
    };
    for (_, size) in &items {
        if x > 0.0 && x + size.x > avail + 0.5 {
            center_line(&mut placed[line_start..], x - gap);
            line_start = placed.len();
            y += line_h + gap;
            x = 0.0;
            line_h = 0.0;
        }
        placed.push(Vec2::new(left + x, y));
        x += size.x + gap;
        line_h = line_h.max(size.y);
    }
    center_line(&mut placed[line_start..], x - gap);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, y + line_h), Sense::hover());

    let mut click = None;
    for ((img, (poll, size)), off) in imgs.iter().zip(&items).zip(placed) {
        let r = Rect::from_min_size(rect.min + off, *size);
        let painter = ui.painter();
        match poll {
            Ok(TexturePoll::Ready { texture }) => {
                let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                painter.image(texture.id, r, uv, Color32::WHITE);
            }
            Ok(TexturePoll::Pending { .. }) => {
                painter.rect_filled(r, 6.0, theme.code_bg);
                painter.text(r.center(), Align2::CENTER_CENTER, "Loading…", FontId::proportional(13.0), theme.faint);
            }
            Err(_) => {
                painter.rect_filled(r, 6.0, theme.code_bg);
                painter.rect_stroke(r, 6.0, Stroke::new(1.0, theme.border), egui::StrokeKind::Inside);
                let label = if img.alt.is_empty() { &img.src } else { &img.alt };
                let text = format!("{}  {label}", egui_phosphor::regular::IMAGE_BROKEN);
                let galley = painter.layout(text, FontId::proportional(13.0), theme.faint, r.width() - 16.0);
                painter.galley(r.center() - galley.size() / 2.0, galley, theme.faint);
            }
        }
        // Inside a selection: tint.
        if !rc.sel.is_empty() && rc.sel.start <= img.range.start && img.range.end <= rc.sel.end {
            painter.rect_filled(r, 0.0, theme.selection);
        }

        let id = ui.id().with(("img", img.range.start));
        let resp = ui.interact(r, id, Sense::click());
        let resp = match poll {
            Err(e) => resp.on_hover_text(format!("{}\n{e}", img.src)),
            _ => resp,
        };
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if resp.clicked() {
            click = Some(Click::SelectImage(img.range.start));
        }

        if rc.img.selected != Some(img.range.start) {
            continue;
        }
        ui.painter().rect_stroke(r.expand(2.0), 3.0, Stroke::new(2.0, theme.accent), egui::StrokeKind::Outside);
        for (k, corner) in [r.left_top(), r.right_top(), r.left_bottom(), r.right_bottom()].into_iter().enumerate() {
            let right = k % 2 == 1;
            let hr = Rect::from_center_size(corner, Vec2::splat(10.0));
            let h = ui.interact(hr.expand(4.0), id.with(k), Sense::drag());
            ui.painter().rect_filled(hr, 2.0, Color32::WHITE);
            ui.painter().rect_stroke(hr, 2.0, Stroke::new(1.5, theme.accent), egui::StrokeKind::Inside);
            if h.hovered() || h.dragged() {
                let nwse = k == 0 || k == 3;
                ui.ctx().set_cursor_icon(if nwse { CursorIcon::ResizeNwSe } else { CursorIcon::ResizeNeSw });
            }
            if h.drag_started() {
                rc.img.drag = Some((img.range.start, size.x));
            }
            if h.dragged()
                && let Some((s, w)) = &mut rc.img.drag
                && *s == img.range.start
            {
                let dx = h.drag_delta().x;
                *w = (*w + if right { dx } else { -dx }).clamp(16.0, avail);
            }
            if h.drag_stopped()
                && let Some((s, w)) = rc.img.drag.take()
                && s == img.range.start
            {
                click = Some(Click::ResizeImage(img.clone(), w.round()));
            }
        }
        if rc.img.drag.is_some_and(|(s, _)| s == img.range.start) {
            let label = format!("{} × {}", size.x.round(), size.y.round());
            let p = r.right_bottom() + Vec2::new(-6.0, -6.0);
            let g = ui.painter().layout_no_wrap(label, FontId::proportional(12.0), Color32::WHITE);
            let bg = Rect::from_min_size(p - g.size() - Vec2::splat(6.0), g.size() + Vec2::splat(6.0));
            ui.painter().rect_filled(bg, 4.0, Color32::from_black_alpha(180));
            ui.painter().galley(bg.min + Vec2::splat(3.0), g, Color32::WHITE);
        }
    }
    click
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    struct S {
        ed: LiveEditor,
        doc: Doc,
        theme: Theme,
        marks: Vec<(Range<usize>, bool)>,
    }

    type H = Harness<'static, S>;

    fn harness(text: &str) -> H {
        // Start empty: the first frame runs before our fonts are installed.
        let s = S { ed: LiveEditor::default(), doc: Doc::new(String::new(), None), theme: Theme::dark(), marks: vec![] };
        let mut h = Harness::builder().with_size(Vec2::new(700.0, 500.0)).build_ui_state(
            |ui, s: &mut S| {
                ui.set_width(600.0);
                let scroll_to = s.marks.first().map(|m| m.0.start);
                s.ed.ui(ui, &mut s.doc, &s.theme, &View { marks: &s.marks, scroll_to, base_dir: None });
            },
            s,
        );
        h.ctx.set_fonts(crate::fonts::definitions(false));
        h.run();
        h.state_mut().doc = Doc::new(text.into(), None);
        h.state_mut().ed = LiveEditor::default();
        h.run();
        h
    }

    fn text(h: &H) -> &str {
        &h.state().doc.text
    }

    /// Focus the editor with the caret at source offset `at`.
    fn caret(h: &mut H, at: usize) {
        let s = h.state_mut();
        s.doc.sel = Sel::caret(at);
        s.ed.focus_next = true;
        h.run();
    }

    fn typ(h: &mut H, t: &str) {
        h.event(Event::Text(t.into()));
        h.run();
    }

    fn key(h: &mut H, k: Key) {
        h.key_press(k);
        h.run();
    }

    fn key_mod(h: &mut H, m: Modifiers, k: Key) {
        h.key_press_modifiers(m, k);
        h.run();
    }

    fn click(h: &mut H, p: Pos2) {
        h.hover_at(p);
        h.drag_at(p);
        h.drop_at(p);
        h.run();
    }

    fn drag(h: &mut H, from: Pos2, to: Pos2) {
        h.hover_at(from);
        h.run();
        h.drag_at(from);
        h.run();
        for i in 1..=5 {
            h.hover_at(from + (to - from) * (i as f32 / 5.0));
            h.run();
        }
        h.drop_at(to);
        h.run();
    }

    #[test]
    fn click_places_caret_and_types() {
        let mut h = harness("Hello world\n\nSecond");
        assert!(!h.state().ed.editing);
        click(&mut h, Pos2::new(20.0, 18.0));
        assert!(h.state().ed.editing, "click focuses the editor");
        typ(&mut h, "X");
        assert_eq!(text(&h), "HXello world\n\nSecond");
    }

    #[test]
    fn typed_markdown_is_literal() {
        let mut h = harness("Hello");
        caret(&mut h, 5);
        typ(&mut h, " *hi* [x]");
        assert_eq!(text(&h), "Hello \\*hi\\* \\[x\\]");
    }

    #[test]
    fn typing_continues_formatting() {
        let mut h = harness("a **bold** b");
        caret(&mut h, 8); // right after "bold", before the closing **
        typ(&mut h, "er");
        assert_eq!(text(&h), "a **bolder** b");
    }

    #[test]
    fn code_blocks_take_raw_text() {
        let mut h = harness("```\ncode\n```");
        caret(&mut h, 8);
        typ(&mut h, "*x");
        assert_eq!(text(&h), "```\ncode*x\n```");
    }

    #[test]
    fn enter_splits_and_new_paragraphs() {
        let mut h = harness("FirstSecond");
        caret(&mut h, 5);
        key(&mut h, Key::Enter);
        assert_eq!(text(&h), "First\n\nSecond");

        let mut h = harness("One");
        caret(&mut h, 3);
        key(&mut h, Key::Enter);
        typ(&mut h, "Two");
        assert_eq!(text(&h), "One\n\nTwo");
    }

    #[test]
    fn enter_inside_bold_reopens_it() {
        let mut h = harness("**abcd**");
        caret(&mut h, 4);
        key(&mut h, Key::Enter);
        assert_eq!(text(&h), "**ab**\n\n**cd**");
    }

    #[test]
    fn lists_continue_and_end() {
        let mut h = harness("- a");
        caret(&mut h, 3);
        key(&mut h, Key::Enter);
        typ(&mut h, "b");
        assert_eq!(text(&h), "- a\n- b");
        key(&mut h, Key::Enter);
        key(&mut h, Key::Enter);
        typ(&mut h, "x");
        assert_eq!(text(&h), "- a\n- b\n\nx");
    }

    #[test]
    fn backspace_rules() {
        // At the start of a list item: remove the bullet, keep the line separate.
        let mut h = harness("- a\n- b");
        caret(&mut h, 6);
        key(&mut h, Key::Backspace);
        assert_eq!(text(&h), "- a\n\nb");

        // At the start of a paragraph: join with the previous one.
        let mut h = harness("One\n\nTwo");
        caret(&mut h, 5);
        key(&mut h, Key::Backspace);
        assert_eq!(text(&h), "OneTwo");

        // Deleting the only char of a bold word removes its markers.
        let mut h = harness("a **b** c");
        caret(&mut h, 5);
        key(&mut h, Key::Backspace);
        assert_eq!(text(&h), "a  c");

        // After a divider: Backspace removes the divider.
        let mut h = harness("One\n\n---\n\nTwo");
        caret(&mut h, 10);
        key(&mut h, Key::Backspace);
        assert_eq!(text(&h), "One\n\nTwo");
    }

    #[test]
    fn arrows_and_shift_selection() {
        let mut h = harness("One\n\nTwo");
        caret(&mut h, 3);
        key(&mut h, Key::ArrowRight);
        assert_eq!(h.state().doc.sel.head, 5, "right at end of block → next block");
        key(&mut h, Key::ArrowLeft);
        assert_eq!(h.state().doc.sel.head, 3);
        key(&mut h, Key::ArrowDown);
        assert!((5..=8).contains(&h.state().doc.sel.head), "down → second paragraph");
        key(&mut h, Key::ArrowUp);
        assert!(h.state().doc.sel.head <= 3);
        caret(&mut h, 0);
        key_mod(&mut h, Modifiers::SHIFT, Key::ArrowRight);
        key_mod(&mut h, Modifiers::SHIFT, Key::ArrowRight);
        assert_eq!(h.state().doc.sel, Sel { anchor: 0, head: 2 });
    }

    #[test]
    fn drag_selects_across_blocks() {
        let mut h = harness("Hello world\n\nSecond line");
        drag(&mut h, Pos2::new(20.0, 18.0), Pos2::new(60.0, 58.0));
        let r = h.state().doc.sel.range();
        assert!(r.start <= 2 && r.end > 13, "selection spans both paragraphs: {r:?}");
        typ(&mut h, "Z");
        assert!(text(&h).starts_with('H') && text(&h).contains('Z') && !text(&h).contains("\n"), "{}", text(&h));
    }

    #[test]
    fn select_all_and_replace() {
        let mut h = harness("# Title\n\nBody **text**");
        caret(&mut h, 0);
        key_mod(&mut h, Modifiers::COMMAND, Key::A);
        typ(&mut h, "x");
        assert_eq!(text(&h), "x");
    }

    #[test]
    fn pending_bold() {
        let mut h = harness("Hi ");
        caret(&mut h, 3);
        h.state_mut().ed.set_pending("**");
        typ(&mut h, "x");
        typ(&mut h, "y");
        assert_eq!(text(&h), "Hi **xy**");
    }

    #[test]
    fn copy_gives_plain_text() {
        let mut h = harness("a **bold** b\n\nnext");
        h.state_mut().doc.sel = Sel { anchor: 0, head: 18 };
        h.run();
        assert_eq!(h.state().ed.selected_text(&h.state().doc), "a bold b\nnext");
    }

    #[test]
    fn image_select_resize_delete() {
        // No loaders in tests → the image draws as a 320×64 placeholder at the top-left.
        let mut h = harness("![A](x.png)\n\nAfter");
        click(&mut h, Pos2::new(100.0, 30.0));
        assert!(h.state().ed.img.selected.is_some(), "click selects the image");

        let handle = Pos2::new(8.0 + 320.0, 8.0 + 64.0);
        drag(&mut h, handle, handle + Vec2::new(50.0, 0.0));
        let t = text(&h).to_owned();
        let tag = &images::find_img_tags(&t)[0];
        assert_eq!((tag.src.as_str(), tag.alt.as_str()), ("x.png", "A"));
        let Some(Width::Px(w)) = tag.width else { panic!("no width in {t}") };
        assert!((360.0..=380.0).contains(&w), "width {w}");

        key(&mut h, Key::Delete);
        assert_eq!(text(&h), "\n\nAfter");
    }

    fn select(h: &mut H, a: usize, b: usize) {
        let s = h.state_mut();
        s.doc.sel = Sel { anchor: a, head: b };
        s.ed.focus_next = true;
        h.run();
    }

    #[test]
    fn bubble_toolbar_formats_selection() {
        use egui_phosphor::regular as icon;
        let mut h = harness("Hello **bold** world");
        // No selection → no floating toolbar.
        caret(&mut h, 2);
        assert!(h.query_by_label(icon::TEXT_ITALIC).is_none());

        select(&mut h, 0, 5);
        h.get_by_label(icon::TEXT_ITALIC).click();
        h.run();
        assert_eq!(h.state_mut().ed.format.take(), Some(Format::Wrap("*")));

        // Block type dropdown.
        select(&mut h, 0, 5);
        h.get_by_label_contains("Text").click();
        h.run();
        h.get_by_label_contains("Heading 2").click();
        h.run();
        assert_eq!(h.state_mut().ed.format.take(), Some(Format::Heading(2)));
    }

    #[test]
    fn color_pending_and_picker() {
        let red = markdown::COLORS[0].1;
        let mut h = harness("Hi ");
        caret(&mut h, 3);
        h.state_mut().ed.set_pending_color(Some(red));
        typ(&mut h, "x");
        typ(&mut h, "y");
        assert_eq!(text(&h), format!("Hi {}xy</span>", markdown::color_open_tag(red)));

        // Floating toolbar: open the color menu, pick "Default"; then clear formatting.
        let mut h = harness("Hello world");
        select(&mut h, 0, 5);
        h.get_by_label(egui_phosphor::regular::TEXT_A_UNDERLINE).click();
        h.run();
        h.get_by_label("Default").click();
        h.run();
        assert_eq!(h.state_mut().ed.format.take(), Some(Format::Color(None)));
        select(&mut h, 0, 5);
        h.get_by_label(egui_phosphor::regular::TEXT_T_SLASH).click();
        h.run();
        assert_eq!(h.state_mut().ed.format.take(), Some(Format::Clear));
    }

    #[test]
    fn find_marks_render() {
        let mut h = harness("Hello **world**, world!");
        h.state_mut().marks = vec![(8..13, true), (16..21, false)];
        h.run();
        caret(&mut h, 3);
        assert!(h.state().ed.editing);
    }
}
