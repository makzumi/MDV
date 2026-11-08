//! The two editing surfaces:
//!
//! * [`LiveEditor`]: every block is rendered; the block holding the caret
//!   turns into an inline text editor showing its (dimmed) markdown syntax.
//! * [`SourceEditor`]: the whole file as highlighted monospace text.
//!
//! Both read and write the same [`Doc`] (text + selection), so switching
//! between them is lossless and keeps the caret where it was.

use crate::doc::{Doc, Sel, ordered_prefix_len};
use crate::images::{self, ImageRef, Width};
use crate::markdown::{self, Block, BlockKind, Marker, RowKind, Span};
use crate::theme::{BASE, MONO, Theme};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, Event, FontId, Frame, Id, Key, Margin, Modifiers, Pos2, Rect,
    Sense, Stroke, TextBuffer, TextEdit, Ui, Vec2,
    text::{CCursor, CCursorRange, LayoutJob},
};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

/// Find-highlight ranges in the source: (range, is_current). Sorted, disjoint.
pub type Marks = [(Range<usize>, bool)];

/// Per-frame context from the app.
pub struct View<'a> {
    pub marks: &'a Marks,
    /// Scroll this source offset into view (one-shot, e.g. current find match).
    pub scroll_to: Option<usize>,
    /// Folder of the open file, for resolving relative image paths.
    pub base_dir: Option<&'a Path>,
}

/// The marks overlapping `range`.
fn marks_in(marks: &Marks, range: Range<usize>) -> &Marks {
    let lo = marks.partition_point(|(m, _)| m.end <= range.start);
    let hi = marks.partition_point(|(m, _)| m.start < range.end.max(range.start + 1));
    &marks[lo..hi.max(lo)]
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn char_index(s: &str, byte: usize) -> usize {
    s[..byte.min(s.len())].chars().count()
}

fn byte_index(s: &str, ch: usize) -> usize {
    s.char_indices().nth(ch).map_or(s.len(), |(i, _)| i)
}

/// Consume a key press only if the modifiers match *exactly*
/// (egui's `consume_key` ignores extra Shift/Alt, which would eat Shift+Arrow).
fn consume_exact(ctx: &egui::Context, mods: Modifiers, key: Key) -> bool {
    ctx.input_mut(|i| {
        let mut hit = false;
        i.events.retain(|e| match e {
            Event::Key { key: k, pressed: true, modifiers, .. } if *k == key && *modifiers == mods && !hit => {
                hit = true;
                false
            }
            _ => true,
        });
        hit
    })
}

/// Caches the highlighted `LayoutJob` for the text being edited so we only
/// re-run the markdown parser when the text (or theme) actually changes.
#[derive(Default)]
pub struct HlCache {
    key: u64,
    job: Option<LayoutJob>,
}

impl HlCache {
    fn job(&mut self, text: &str, theme: &Theme, live: bool, base: usize, marks: &Marks) -> LayoutJob {
        let marks = marks_in(marks, base..base + text.len());
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (text, theme.dark, live, base, marks).hash(&mut h);
        let key = h.finish();
        if self.job.is_none() || key != self.key {
            let mut job = LayoutJob::default();
            if text.is_empty() {
                // Empty section so the caret gets the right line height.
                job.append("", 0.0, theme.source(&markdown::Hl::default(), live));
            }
            for (r, hl) in markdown::highlight(text) {
                append_marked(&mut job, text, r, base, marks, theme.source(&hl, live), theme);
            }
            self.key = key;
            self.job = Some(job);
        }
        self.job.clone().unwrap()
    }
}

/// Append `text[r]` with `fmt`, giving find matches a highlight background.
fn append_marked(
    job: &mut LayoutJob,
    text: &str,
    r: Range<usize>,
    base: usize,
    marks: &Marks,
    fmt: egui::TextFormat,
    theme: &Theme,
) {
    let mut pos = r.start;
    while pos < r.end {
        let g = base + pos;
        let i = marks.partition_point(|(m, _)| m.end <= g);
        let (end, bg) = match marks.get(i) {
            Some((m, cur)) if m.start <= g => ((m.end - base).min(r.end), Some(*cur)),
            Some((m, _)) => ((m.start - base).min(r.end), None),
            None => (r.end, None),
        };
        let mut f = fmt.clone();
        if let Some(cur) = bg {
            f.background = if cur { theme.find_current } else { theme.find };
        }
        job.append(&text[pos..end], 0.0, f);
        pos = end;
    }
}

struct EditOut {
    changed: bool,
    sel: Option<Sel>,
    first_row: bool,
    last_row: bool,
    lost_focus: bool,
}

/// A TextEdit over `buf` (which corresponds to doc bytes starting at `base`)
/// whose cursor is driven by / written back to a global [`Sel`].
#[allow(clippy::too_many_arguments)]
fn synced_text_edit(
    ui: &mut Ui,
    id: Id,
    buf: &mut String,
    base: usize,
    sel: Sel,
    hl: &mut HlCache,
    theme: &Theme,
    live: bool,
    focus: bool,
    view: &View,
) -> EditOut {
    let ctx = ui.ctx().clone();
    let end = base + buf.len();
    let local = |b: usize| CCursor::new(char_index(buf, b.clamp(base, end) - base));
    let mut state = TextEdit::load_state(&ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange {
        primary: local(sel.head),
        secondary: local(sel.anchor),
        h_pos: state.cursor.char_range().and_then(|r| r.h_pos),
    }));
    TextEdit::store_state(&ctx, id, state);
    // Must happen *before* `show`: requesting focus afterwards resets egui's
    // focus-lock filter, and the next arrow key would then move focus away.
    // egui only installs the TextEdit's arrow-key filter once the widget has
    // held focus for a full frame, so force one more frame right away; otherwise
    // an arrow key arriving next would be treated as focus navigation.
    if focus {
        ctx.memory_mut(|m| m.request_focus(id));
        ctx.request_repaint();
    }

    let mut layouter = |ui: &Ui, text: &dyn TextBuffer, wrap: f32| -> Arc<egui::Galley> {
        let mut job = hl.job(text.as_str(), theme, live, base, view.marks);
        job.wrap.max_width = wrap;
        ui.fonts_mut(|f| f.layout_job(job))
    };

    let out = TextEdit::multiline(buf)
        .id(id)
        .frame(Frame::NONE)
        .margin(0.0)
        .desired_width(ui.available_width())
        .desired_rows(1)
        .layouter(&mut layouter)
        .show(ui);

    let resp = &out.response.response;

    if let Some(b) = view.scroll_to
        && (base..=end).contains(&b)
    {
        let c = CCursor::new(char_index(buf, b - base));
        let r = out.galley.pos_from_cursor(c).translate(out.galley_pos.to_vec2());
        ui.scroll_to_rect(r.expand(60.0), Some(egui::Align::Center));
    }

    let sel = out.cursor_range.map(|r| Sel {
        anchor: base + byte_index(buf, r.secondary.index.0),
        head: base + byte_index(buf, r.primary.index.0),
    });
    let (first_row, last_row) = match out.cursor_range {
        Some(r) => {
            let lc = out.galley.layout_from_cursor(r.primary);
            (lc.row == 0, lc.row + 1 >= out.galley.rows.len())
        }
        None => (true, true),
    };
    EditOut { changed: resp.changed(), sel, first_row, last_row, lost_focus: resp.lost_focus() }
}

// ---------------------------------------------------------------------------
// Source view
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct SourceEditor {
    buf: String,
    rev: Option<u64>,
    hl: HlCache,
    pub focus_next: bool,
}

pub const SOURCE_ID: &str = "mdv-source";

impl SourceEditor {
    pub fn ui(&mut self, ui: &mut Ui, doc: &mut Doc, theme: &Theme, view: &View) {
        if self.rev != Some(doc.rev) {
            self.buf.clone_from(&doc.text);
            self.rev = Some(doc.rev);
        }
        let focus = std::mem::take(&mut self.focus_next);
        let out = synced_text_edit(ui, Id::new(SOURCE_ID), &mut self.buf, 0, doc.sel, &mut self.hl, theme, false, focus, view);
        let sel = out.sel.unwrap_or(doc.sel);
        if out.changed {
            doc.set_text(self.buf.clone(), sel);
            self.rev = Some(doc.rev);
        } else {
            doc.sel = sel;
        }
    }
}

// ---------------------------------------------------------------------------
// Live (WYSIWYG-ish) view
// ---------------------------------------------------------------------------

pub const LIVE_ID: &str = "mdv-live";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Active {
    Block(usize),
    /// Caret is in blank space between blocks; `before` = index of the next block.
    Gap { before: usize },
}

enum Nav {
    Enter { shift: bool },
    MergePrev,
    MergeNext,
    Prev,
    Next,
}

/// Something the user clicked in a rendered (non-editing) block.
enum Click {
    Caret(usize),
    Link(String),
    Task(usize),
    /// Select the image whose syntax starts at this offset.
    SelectImage(usize),
    /// A resize handle was released: new display width in points.
    ResizeImage(ImageRef, f32),
}

/// Image selection / resize-drag state.
#[derive(Default)]
struct ImgUi {
    selected: Option<usize>,
    /// (image start offset, current width) while a handle is dragged.
    drag: Option<(usize, f32)>,
}

/// Everything the block renderers need besides the block itself.
struct Rc<'a> {
    theme: &'a Theme,
    marks: &'a Marks,
    base_dir: Option<&'a Path>,
    img: &'a mut ImgUi,
}

pub struct LiveEditor {
    blocks: Vec<Block>,
    rev: Option<u64>,
    /// `true` while a block is being edited (an editor has focus).
    pub editing: bool,
    /// Request keyboard focus for the inline editor on the next frame.
    pub focus_next: bool,
    first_row: bool,
    last_row: bool,
    hl: HlCache,
    img: ImgUi,
}

impl Default for LiveEditor {
    fn default() -> Self {
        Self {
            blocks: vec![],
            rev: None,
            editing: false,
            focus_next: false,
            first_row: true,
            last_row: true,
            hl: HlCache::default(),
            img: ImgUi::default(),
        }
    }
}

impl LiveEditor {
    fn sync(&mut self, doc: &Doc) {
        if self.rev != Some(doc.rev) {
            self.blocks = markdown::parse_blocks(&doc.text);
            self.rev = Some(doc.rev);
        }
    }

    fn find_image(&self, start: usize) -> Option<ImageRef> {
        self.blocks
            .iter()
            .flat_map(|b| &b.rows)
            .flat_map(|r| {
                let cells = match &r.kind {
                    RowKind::Table(t) => t.iter().flat_map(|tr| tr.cells.iter().flatten()).collect::<Vec<_>>(),
                    _ => vec![],
                };
                r.spans.iter().chain(cells)
            })
            .filter_map(|s| s.image.as_deref())
            .find(|i| i.range.start == start)
            .cloned()
    }

    /// Delete / Escape while an image is selected (and no text is being edited).
    fn handle_image_keys(&mut self, ctx: &egui::Context, doc: &mut Doc) {
        let Some(start) = self.img.selected else { return };
        if self.editing {
            return;
        }
        if consume_exact(ctx, Modifiers::NONE, Key::Escape) {
            self.img.selected = None;
            return;
        }
        if let Some(img) = self.find_image(start)
            && (consume_exact(ctx, Modifiers::NONE, Key::Delete) || consume_exact(ctx, Modifiers::NONE, Key::Backspace))
        {
            doc.replace(img.range.clone(), "", Sel::caret(img.range.start), false);
            self.img.selected = None;
        }
    }

    fn active(&self, head: usize) -> Active {
        if let Some(i) = self.blocks.iter().position(|b| b.range.start <= head && head <= b.range.end) {
            return Active::Block(i);
        }
        let before = self.blocks.iter().position(|b| b.range.start > head).unwrap_or(self.blocks.len());
        Active::Gap { before }
    }

    /// (editable range, end of previous block, start of next block)
    fn neighbours(&self, active: Active, head: usize) -> (Range<usize>, Option<usize>, Option<usize>) {
        match active {
            Active::Block(i) => (
                self.blocks[i].range.clone(),
                i.checked_sub(1).map(|j| self.blocks[j].range.end),
                self.blocks.get(i + 1).map(|b| b.range.start),
            ),
            Active::Gap { before } => (
                head..head,
                before.checked_sub(1).map(|j| self.blocks[j].range.end),
                self.blocks.get(before).map(|b| b.range.start),
            ),
        }
    }

    /// Keys that cross block boundaries are handled here, before the inline
    /// TextEdit sees them.
    fn handle_keys(&mut self, ctx: &egui::Context, doc: &mut Doc) {
        if !self.editing || !ctx.memory(|m| m.has_focus(Id::new(LIVE_ID))) {
            return;
        }
        let head = doc.sel.head;
        let active = self.active(head);
        let (range, prev, next) = self.neighbours(active, head);
        let caret = doc.sel.is_empty();
        let n = Modifiers::NONE;

        let nav = if consume_exact(ctx, n, Key::Enter) {
            Some(Nav::Enter { shift: false })
        } else if consume_exact(ctx, Modifiers::SHIFT, Key::Enter) {
            Some(Nav::Enter { shift: true })
        } else if caret && head == range.start && (prev.is_some() || head > 0) && consume_exact(ctx, n, Key::Backspace) {
            Some(Nav::MergePrev)
        } else if caret && head == range.end && (next.is_some() || head < doc.text.len()) && consume_exact(ctx, n, Key::Delete) {
            Some(Nav::MergeNext)
        } else if prev.is_some()
            && ((self.first_row && consume_exact(ctx, n, Key::ArrowUp))
                || (caret && head == range.start && consume_exact(ctx, n, Key::ArrowLeft)))
        {
            Some(Nav::Prev)
        } else if next.is_some()
            && ((self.last_row && consume_exact(ctx, n, Key::ArrowDown))
                || (caret && head == range.end && consume_exact(ctx, n, Key::ArrowRight)))
        {
            Some(Nav::Next)
        } else {
            None
        };

        match nav {
            None => {}
            Some(Nav::Prev) => doc.sel = Sel::caret(prev.unwrap()),
            Some(Nav::Next) => doc.sel = Sel::caret(next.unwrap()),
            Some(Nav::MergePrev) => {
                let from = prev.unwrap_or(0);
                doc.replace(from..head, "", Sel::caret(from), false);
            }
            Some(Nav::MergeNext) => {
                let to = next.unwrap_or(doc.text.len());
                doc.replace(head..to, "", Sel::caret(head), false);
            }
            Some(Nav::Enter { shift }) => {
                let kind = match active {
                    Active::Block(i) => self.blocks[i].kind,
                    // Markdown has no empty paragraphs; Enter in blank space does nothing.
                    Active::Gap { .. } if !shift => return,
                    Active::Gap { .. } => BlockKind::Paragraph,
                };
                smart_enter(doc, kind, shift);
            }
        }
        self.sync(doc);
    }

    pub fn ui(&mut self, ui: &mut Ui, doc: &mut Doc, theme: &Theme, view: &View) {
        self.sync(doc);
        self.handle_keys(ui.ctx(), doc);
        self.handle_image_keys(ui.ctx(), doc);
        self.sync(doc);

        let width = ui.available_width();
        let active = self.editing.then(|| self.active(doc.sel.head));
        let mut click: Option<Click> = None;
        let focus = std::mem::take(&mut self.focus_next);

        ui.spacing_mut().item_spacing.y = 0.0;
        let n = self.blocks.len();
        for i in 0..=n {
            if active == Some(Active::Gap { before: i }) {
                if i > 0 {
                    ui.add_space(block_gap(BlockKind::Paragraph));
                }
                let head = doc.sel.head;
                self.edit_range(ui, doc, theme, head..head, BlockKind::Paragraph, focus, view);
            }
            if i == n {
                break;
            }
            let kind = self.blocks[i].kind;
            if i > 0 || active == Some(Active::Gap { before: 0 }) {
                ui.add_space(block_gap(kind));
            }
            let top = ui.cursor().top();
            let range = self.blocks[i].range.clone();
            if active == Some(Active::Block(i)) {
                self.edit_range(ui, doc, theme, range.clone(), kind, focus, view);
            } else {
                let mut rc = Rc {
                    theme,
                    marks: marks_in(view.marks, range.clone()),
                    base_dir: view.base_dir,
                    img: &mut self.img,
                };
                if let Some(c) = render_block(ui, &self.blocks[i], &mut rc, width) {
                    click = Some(c);
                }
                if let Some(b) = view.scroll_to
                    && range.contains(&b)
                {
                    let left = ui.min_rect().left();
                    let r = Rect::from_min_max(Pos2::new(left, top), Pos2::new(left + width, ui.cursor().top()));
                    ui.scroll_to_rect(r.expand(40.0), Some(egui::Align::Center));
                }
            }
        }

        // Click in the empty space below the document → caret at the end.
        let rest = ui.available_size_before_wrap().max(Vec2::new(width, 120.0));
        let (_, resp) = ui.allocate_exact_size(rest, Sense::click());
        if resp.clicked() {
            click = Some(Click::Caret(doc.text.len()));
        }

        match click {
            Some(Click::Caret(at)) => {
                doc.sel = Sel::caret(at);
                self.editing = true;
                self.focus_next = true;
                self.img.selected = None;
                ui.ctx().request_repaint();
            }
            Some(Click::Link(url)) => ui.ctx().open_url(egui::OpenUrl::new_tab(url)),
            Some(Click::Task(at)) => doc.toggle_task(at),
            Some(Click::SelectImage(start)) => {
                self.img.selected = Some(start);
                self.editing = false;
                ui.ctx().memory_mut(|m| m.surrender_focus(Id::new(LIVE_ID)));
            }
            Some(Click::ResizeImage(img, w)) => {
                let md = images::to_markdown(&img.src, &img.alt, Some(Width::Px(w)));
                doc.replace(img.range.clone(), &md, Sel::caret(img.range.start), false);
                self.img.selected = Some(img.range.start);
            }
            None => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn edit_range(
        &mut self,
        ui: &mut Ui,
        doc: &mut Doc,
        theme: &Theme,
        range: Range<usize>,
        kind: BlockKind,
        focus: bool,
        view: &View,
    ) {
        let mut buf = doc.text[range.clone()].to_owned();
        let is_code = kind == BlockKind::Code;
        let frame = if is_code {
            Frame::NONE.fill(theme.code_bg).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(16, 14))
        } else {
            Frame::NONE
        };
        let out = frame
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                synced_text_edit(ui, Id::new(LIVE_ID), &mut buf, range.start, doc.sel, &mut self.hl, theme, true, focus, view)
            })
            .inner;

        let sel = out.sel.unwrap_or(doc.sel);
        if out.changed {
            doc.replace(range, &buf, sel, true);
        } else {
            doc.sel = sel;
        }
        self.first_row = out.first_row;
        self.last_row = out.last_row;
        if out.lost_focus {
            self.editing = false;
        }
    }
}

fn block_gap(kind: BlockKind) -> f32 {
    match kind {
        BlockKind::Heading(1 | 2) => 28.0,
        BlockKind::Heading(_) => 20.0,
        _ => 12.0,
    }
}

/// Enter key behaviour, markdown-aware (continue lists/quotes, split paragraphs).
fn smart_enter(doc: &mut Doc, kind: BlockKind, shift: bool) {
    let r = doc.sel.range();
    let t = &doc.text;
    let line_start = t[..r.start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = t[r.end..].find('\n').map_or(t.len(), |i| r.end + i);
    let line = &t[line_start..line_end];
    let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let trimmed = &line[indent.len()..];

    // Insert `s` replacing the selection.
    let insert = |doc: &mut Doc, s: String| {
        let caret = r.start + s.len();
        doc.replace(r.clone(), &s, Sel::caret(caret), false);
    };
    // Replace the whole current line with a blank line (ends a list/quote).
    let end_container = |doc: &mut Doc| {
        doc.replace(line_start..line_end, "\n", Sel::caret(line_start + 1), false);
    };

    if shift {
        return insert(doc, "\n".into());
    }
    match kind {
        BlockKind::Code | BlockKind::Html | BlockKind::Table => insert(doc, format!("\n{indent}")),
        BlockKind::List => {
            let (marker_len, next) = if let Some(n) = ordered_prefix_len(trimmed) {
                let num: u64 = trimmed[..n - 2].parse().unwrap_or(1);
                let delim = &trimmed[n - 2..n - 1];
                (n, format!("{}{delim} ", num + 1))
            } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
                (2, trimmed[..2].to_owned())
            } else {
                return insert(doc, format!("\n{indent}"));
            };
            let mut rest = &trimmed[marker_len..];
            let mut next = next;
            for task in ["[ ] ", "[x] ", "[X] "] {
                if let Some(r) = rest.strip_prefix(task) {
                    rest = r;
                    next.push_str("[ ] ");
                    break;
                }
            }
            if rest.trim().is_empty() {
                end_container(doc)
            } else {
                insert(doc, format!("\n{indent}{next}"))
            }
        }
        BlockKind::Quote => {
            let prefix_len = trimmed.len() - trimmed.trim_start_matches(['>', ' ']).len();
            let prefix = format!("{indent}{}", &trimmed[..prefix_len]);
            if trimmed[prefix_len..].trim().is_empty() {
                end_container(doc)
            } else {
                insert(doc, format!("\n{prefix}"))
            }
        }
        _ => insert(doc, "\n\n".into()),
    }
}

// ---------------------------------------------------------------------------
// Rendering non-active blocks
// ---------------------------------------------------------------------------

struct Laid {
    galley: Arc<egui::Galley>,
    /// char index → source byte (len = chars + 1)
    map: Vec<usize>,
    links: Vec<(Range<usize>, String)>,
}

fn lay_out(ui: &Ui, spans: &[Span], theme: &Theme, heading: u8, code: bool, wrap: f32, bold: bool) -> Laid {
    lay_out_aligned(ui, spans, theme, heading, code, wrap, bold, false)
}

/// `center`: rows are centered around x = 0, so paint at `left + wrap / 2`.
#[allow(clippy::too_many_arguments)]
fn lay_out_aligned(
    ui: &Ui,
    spans: &[Span],
    theme: &Theme,
    heading: u8,
    code: bool,
    wrap: f32,
    bold: bool,
    center: bool,
) -> Laid {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap;
    if center {
        job.halign = egui::Align::Center;
    }
    let mut map = Vec::new();
    let mut links = Vec::new();
    let mut chars = 0;
    for s in spans.iter().filter(|s| s.image.is_none()) {
        let mut style = s.style.clone();
        style.bold |= bold;
        let fmt = theme.rendered(&style, heading, code);
        job.append(&s.text, 0.0, fmt);
        let start = chars;
        for (bi, _) in s.text.char_indices() {
            map.push(if s.exact { s.src + bi } else { s.src });
            chars += 1;
        }
        if let Some(url) = &s.style.link {
            links.push((start..chars, url.clone()));
        }
    }
    let end = spans.last().map_or(0, |s| if s.exact { s.src + s.text.len() } else { s.src });
    map.push(end);
    if job.text.is_empty() {
        job.append("", 0.0, theme.rendered(&Default::default(), heading, code));
    }
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    Laid { galley, map, links }
}

/// Paint a laid-out galley at `pos` inside `resp`'s rect and translate a click.
/// Only pointer positions inside `area` count (lets table cells share one response).
fn galley_click(ui: &Ui, laid: &Laid, pos: Pos2, resp: &egui::Response, area: Rect, rc: &Rc) -> Option<Click> {
    let theme = rc.theme;
    paint_marks(ui, laid, pos, rc);
    ui.painter().galley(pos, laid.galley.clone(), theme.text);

    let char_at = |p: Pos2| laid.galley.cursor_from_pos(p - pos).index.0.min(laid.map.len() - 1);
    let link_at = |c: usize| laid.links.iter().find(|(r, _)| r.contains(&c)).map(|(_, u)| u.clone());

    if let Some(h) = resp.hover_pos()
        && area.contains(h)
    {
        if let Some(url) = link_at(char_at(h)) {
            let ctrl = ui.input(|i| i.modifiers.command);
            ui.ctx().set_cursor_icon(if ctrl { CursorIcon::PointingHand } else { CursorIcon::Text });
            resp.clone().on_hover_text_at_pointer(format!("{url}\nCtrl+click to open"));
        } else {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
        }
    }
    if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && area.contains(p)
    {
        let c = char_at(p);
        if ui.input(|i| i.modifiers.command)
            && let Some(url) = link_at(c)
        {
            return Some(Click::Link(url));
        }
        return Some(Click::Caret(laid.map[c]));
    }
    None
}

/// Paint find-match backgrounds behind the chars whose source offset is marked.
fn paint_marks(ui: &Ui, laid: &Laid, pos: Pos2, rc: &Rc) {
    if rc.marks.is_empty() || laid.map.len() < 2 {
        return;
    }
    let mark_of = |src: usize| {
        let i = rc.marks.partition_point(|(m, _)| m.end <= src);
        rc.marks.get(i).filter(|(m, _)| m.start <= src).map(|(_, cur)| *cur)
    };
    let mut cur: Option<(Rect, bool)> = None;
    let flush = |r: Option<(Rect, bool)>| {
        if let Some((r, c)) = r {
            let color = if c { rc.theme.find_current } else { rc.theme.find };
            ui.painter().rect_filled(r.translate(pos.to_vec2()), 2.0, color);
        }
    };
    for c in 0..laid.map.len() - 1 {
        let Some(is_cur) = mark_of(laid.map[c]) else {
            flush(cur.take());
            continue;
        };
        let a = laid.galley.pos_from_cursor(CCursor::new(c));
        let b = laid.galley.pos_from_cursor(CCursor::new(c + 1));
        if (a.min.y - b.min.y).abs() > 0.5 {
            continue; // wraps to the next row
        }
        let r = Rect::from_min_max(a.min, Pos2::new(b.min.x, a.max.y));
        match &mut cur {
            Some((acc, ac)) if *ac == is_cur && (acc.max.x - r.min.x).abs() < 0.5 && (acc.min.y - r.min.y).abs() < 0.5 => {
                *acc = acc.union(r);
            }
            _ => {
                flush(cur.take());
                cur = Some((r, is_cur));
            }
        }
    }
    flush(cur);
}

const LIST_INDENT: f32 = 26.0;
const QUOTE_INDENT: f32 = 20.0;

fn render_block(ui: &mut Ui, block: &Block, rc: &mut Rc, width: f32) -> Option<Click> {
    let theme = rc.theme;
    let mut click = None;
    for (ri, row) in block.rows.iter().enumerate() {
        if ri > 0 {
            ui.add_space(match row.kind {
                RowKind::Text if row.marker.is_some() => 4.0,
                _ => 10.0,
            });
        }
        let c = match &row.kind {
            RowKind::Rule => {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::click());
                ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, theme.border));
                resp.clicked().then_some(Click::Caret(row.src))
            }
            RowKind::Code => {
                let pad = Vec2::new(16.0, 14.0);
                let laid = lay_out(ui, &row.spans, theme, 0, true, width - pad.x * 2.0, false);
                let size = Vec2::new(width, laid.galley.size().y.max(MONO * 1.4) + pad.y * 2.0);
                let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                ui.painter().rect_filled(rect, 8.0, theme.code_bg);
                ui.painter().rect_stroke(rect, 8.0, Stroke::new(1.0, theme.border), egui::StrokeKind::Inside);
                galley_click(ui, &laid, rect.min + pad, &resp, rect, rc)
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
    let laid = lay_out_aligned(ui, &row.spans, theme, row.heading, false, wrap, false, row.center);
    let first_row_h = laid.galley.rows.first().map_or(BASE * 1.6, |r| r.height());
    let size = Vec2::new(width, laid.galley.size().y.max(first_row_h));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
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

    let mut laid = laid;
    if matches!(row.marker, Some(Marker::Task { checked: true, .. })) {
        // Dim completed tasks.
        let mut job = (*laid.galley.job).clone();
        for s in &mut job.sections {
            s.format.color = theme.faint;
            s.format.strikethrough = Stroke::new(1.0, theme.faint);
        }
        laid.galley = ui.fonts_mut(|f| f.layout_job(job));
    }
    let x = rect.left() + left + if row.center { wrap / 2.0 } else { 0.0 };
    let galley_click = galley_click(ui, &laid, Pos2::new(x, rect.top()), &resp, rect, rc);
    click.or(galley_click)
}

fn render_table(ui: &mut Ui, rows: &[markdown::TableRow], rc: &mut Rc, width: f32) -> Option<Click> {
    let theme = rc.theme;
    let cols = rows.iter().map(|r| r.cells.len()).max().unwrap_or(1).max(1);
    let pad = Vec2::new(12.0, 8.0);
    let col_w = width / cols as f32;
    let mut click = None;
    let top = ui.cursor().top();

    for (ri, r) in rows.iter().enumerate() {
        let laid: Vec<Laid> = (0..cols)
            .map(|c| {
                let spans = r.cells.get(c).map_or(&[][..], |v| v.as_slice());
                lay_out(ui, spans, theme, 0, false, col_w - pad.x * 2.0, r.header)
            })
            .collect();
        let h = laid.iter().map(|l| l.galley.size().y).fold(BASE * 1.6, f32::max) + pad.y * 2.0;
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, h), Sense::click());
        if r.header {
            ui.painter().rect_filled(rect, CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 }, theme.code_bg);
        } else if ri % 2 == 0 {
            ui.painter().rect_filled(rect, 0.0, theme.panel);
        }
        for (c, l) in laid.iter().enumerate() {
            let pos = Pos2::new(rect.left() + c as f32 * col_w, rect.top()) + pad;
            let cell_rect = Rect::from_min_size(Pos2::new(rect.left() + c as f32 * col_w, rect.top()), Vec2::new(col_w, h));
            if let Some(cl) = galley_click(ui, l, pos, &resp, cell_rect, rc) {
                click = Some(cl);
            }
        }
        ui.painter().hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, theme.border));
    }
    let outer = Rect::from_min_max(Pos2::new(ui.min_rect().left(), top), Pos2::new(ui.min_rect().left() + width, ui.cursor().top()));
    ui.painter().rect_stroke(outer, 8.0, Stroke::new(1.0, theme.border), egui::StrokeKind::Inside);
    click
}

/// Draw a row of images (wrapping), with selection outline and resize handles.
fn render_images(ui: &mut Ui, imgs: &[ImageRef], left: f32, width: f32, center: bool, rc: &mut Rc) -> Option<Click> {
    use egui::load::{SizeHint, TexturePoll};
    let theme = rc.theme;
    let avail = (width - left).max(50.0);
    let ctx = ui.ctx().clone();
    let gap = 8.0;

    // Measure.
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

        let id = ui.id().with(("img", img.range.start));
        let resp = ui.interact(r, id, Sense::click());
        let resp = match poll {
            Err(e) => resp.on_hover_text(format!("{}\n{e}", img.src)),
            _ => resp,
        };
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if resp.double_clicked() {
            click = Some(Click::Caret(img.range.start));
        } else if resp.clicked() {
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

    struct S {
        ed: LiveEditor,
        doc: Doc,
        theme: Theme,
        marks: Vec<(Range<usize>, bool)>,
    }

    fn harness(text: &str) -> Harness<'static, S> {
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
        // Fresh editor too, like the app does when it loads a file.
        h.state_mut().doc = Doc::new(text.into(), None);
        h.state_mut().ed = LiveEditor::default();
        h.run();
        h
    }

    fn click(h: &mut Harness<'static, S>, pos: Pos2) {
        h.hover_at(pos);
        h.drag_at(pos);
        h.drop_at(pos);
        h.run();
    }

    fn type_text(h: &mut Harness<'static, S>, t: &str) {
        h.event(Event::Text(t.into()));
        h.run();
    }

    fn key(h: &mut Harness<'static, S>, k: Key) {
        h.key_press(k);
        h.run();
    }

    /// Put the caret at a byte offset and enter editing mode.
    fn caret(h: &mut Harness<'static, S>, at: usize) {
        let s = h.state_mut();
        s.doc.sel = Sel::caret(at);
        s.ed.editing = true;
        s.ed.focus_next = true;
        h.run();
    }

    #[test]
    fn click_enters_editing_and_types() {
        let mut h = harness("Hello world\n\nSecond");
        assert!(!h.state().ed.editing);
        click(&mut h, Pos2::new(20.0, 18.0));
        assert!(h.state().ed.editing, "click should start editing");
        type_text(&mut h, "X");
        // x=20 lands just after the "H": click → caret mapping works.
        assert_eq!(h.state().doc.text, "HXello world\n\nSecond");
        assert!(h.state().doc.dirty);
    }

    #[test]
    fn enter_splits_paragraph_and_types_into_new_block() {
        let mut h = harness("First\n\nSecond");
        caret(&mut h, 5);
        key(&mut h, Key::Enter);
        assert_eq!(h.state().doc.text, "First\n\n\n\nSecond");
        type_text(&mut h, "Middle");
        assert_eq!(h.state().doc.text, "First\n\nMiddle\n\nSecond");
        assert_eq!(markdown::parse_blocks(&h.state().doc.text).len(), 3);
    }

    #[test]
    fn enter_continues_and_ends_lists() {
        let mut h = harness("- a");
        caret(&mut h, 3);
        key(&mut h, Key::Enter);
        type_text(&mut h, "b");
        assert_eq!(h.state().doc.text, "- a\n- b");
        key(&mut h, Key::Enter);
        key(&mut h, Key::Enter);
        type_text(&mut h, "after");
        assert_eq!(h.state().doc.text, "- a\n- b\n\nafter");
    }

    #[test]
    fn ordered_and_task_lists_continue() {
        let mut h = harness("1. one");
        caret(&mut h, 6);
        key(&mut h, Key::Enter);
        assert_eq!(h.state().doc.text, "1. one\n2. ");
        let mut h = harness("- [x] done");
        caret(&mut h, 10);
        key(&mut h, Key::Enter);
        assert_eq!(h.state().doc.text, "- [x] done\n- [ ] ");
    }

    #[test]
    fn arrows_move_between_blocks() {
        let mut h = harness("One\n\nTwo");
        caret(&mut h, 3);
        key(&mut h, Key::ArrowDown);
        assert_eq!(h.state().doc.sel.head, 5, "down from last row goes to next block");
        key(&mut h, Key::ArrowUp);
        assert_eq!(h.state().doc.sel.head, 3, "up from first row goes to previous block end");
        key(&mut h, Key::ArrowRight);
        assert_eq!(h.state().doc.sel.head, 5);
        key(&mut h, Key::ArrowLeft);
        assert_eq!(h.state().doc.sel.head, 3);
    }

    #[test]
    fn backspace_at_block_start_merges() {
        let mut h = harness("One\n\nTwo");
        caret(&mut h, 5);
        key(&mut h, Key::Backspace);
        assert_eq!(h.state().doc.text, "OneTwo");
        assert_eq!(h.state().doc.sel.head, 3);
        h.state_mut().doc.undo();
        assert_eq!(h.state().doc.text, "One\n\nTwo");
    }

    #[test]
    fn typing_in_empty_document() {
        let mut h = harness("");
        click(&mut h, Pos2::new(50.0, 50.0));
        type_text(&mut h, "# Hi");
        assert_eq!(h.state().doc.text, "# Hi");
    }

    fn press(h: &mut Harness<'static, S>, pos: Pos2) {
        h.hover_at(pos);
        h.run();
        h.drag_at(pos);
        h.run();
    }

    fn release(h: &mut Harness<'static, S>, pos: Pos2) {
        h.drop_at(pos);
        h.run();
    }

    #[test]
    fn image_select_resize_delete() {
        // No loaders in tests → the image draws as a 320×64 placeholder at the top-left.
        let mut h = harness("![A](x.png)\n\nAfter");
        click(&mut h, Pos2::new(100.0, 30.0));
        assert!(h.state().ed.img.selected.is_some(), "click selects the image");
        assert!(!h.state().ed.editing);

        // Drag the bottom-right handle 50px to the right.
        let handle = Pos2::new(8.0 + 320.0, 8.0 + 64.0);
        press(&mut h, handle);
        for dx in [10.0, 25.0, 40.0, 50.0] {
            h.hover_at(handle + Vec2::new(dx, 0.0));
            h.run();
        }
        release(&mut h, handle + Vec2::new(50.0, 0.0));
        let text = h.state().doc.text.clone();
        let tag = &images::find_img_tags(&text)[0];
        assert_eq!((tag.src.as_str(), tag.alt.as_str()), ("x.png", "A"));
        let Some(Width::Px(w)) = tag.width else { panic!("no width in {text}") };
        assert!((360.0..=380.0).contains(&w), "width {w}");
        assert!(text.ends_with("\n\nAfter"));

        // Still selected → Delete removes it.
        key(&mut h, Key::Delete);
        assert_eq!(h.state().doc.text, "\n\nAfter");
    }

    #[test]
    fn find_marks_render_without_panicking() {
        let mut h = harness("Hello **world**, world!");
        h.state_mut().marks = vec![(8..13, true), (16..21, false)];
        h.run();
        // And while editing (marks go through the highlighter instead).
        caret(&mut h, 3);
        assert!(h.state().ed.editing);
    }

    #[test]
    fn escape_leaves_editing() {
        let mut h = harness("One");
        caret(&mut h, 1);
        assert!(h.state().ed.editing);
        key(&mut h, Key::Escape);
        assert!(!h.state().ed.editing);
    }
}
