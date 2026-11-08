//! Shared editor pieces and the Source view ([`SourceEditor`]: the whole file
//! as highlighted monospace text). The WYSIWYG Editor view lives in `live.rs`.
//!
//! Both views read and write the same [`Doc`] (text + selection), so switching
//! between them is lossless and keeps the caret where it was.

use crate::doc::{Doc, Sel};
use crate::markdown;
use crate::theme::Theme;
use eframe::egui::{
    self, Frame, Id, TextBuffer, TextEdit, Ui,
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
pub fn marks_in(marks: &Marks, range: Range<usize>) -> &Marks {
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
    synced: &mut Option<Sel>,
) -> EditOut {
    let sel_in = sel;
    let ctx = ui.ctx().clone();
    let end = base + buf.len();
    // Push the selection into the widget only when it changed from outside
    // (toolbar, find, undo, view switch). Overwriting it every frame would
    // clobber a mouse drag in progress: the widget isn't focused until the
    // button is released, so it reports no cursor during the drag.
    if *synced != Some(sel) {
        let local = |b: usize| CCursor::new(char_index(buf, b.clamp(base, end) - base));
        let mut state = TextEdit::load_state(&ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange {
            primary: local(sel.head),
            secondary: local(sel.anchor),
            h_pos: None,
        }));
        TextEdit::store_state(&ctx, id, state);
    }
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

    // The widget's own selection (also valid while unfocused, e.g. mid-drag).
    let sel = out.state.cursor.char_range().or(out.cursor_range).map(|r| Sel {
        anchor: base + byte_index(buf, r.secondary.index.0),
        head: base + byte_index(buf, r.primary.index.0),
    });
    *synced = sel.or(Some(sel_in));
    EditOut { changed: resp.changed(), sel }
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
    /// Last selection exchanged with the TextEdit.
    synced: Option<Sel>,
}

pub const SOURCE_ID: &str = "mdv-source";

impl SourceEditor {
    pub fn ui(&mut self, ui: &mut Ui, doc: &mut Doc, theme: &Theme, view: &View) {
        if self.rev != Some(doc.rev) {
            self.buf.clone_from(&doc.text);
            self.rev = Some(doc.rev);
        }
        let focus = std::mem::take(&mut self.focus_next);
        let out = synced_text_edit(
            ui,
            Id::new(SOURCE_ID),
            &mut self.buf,
            0,
            doc.sel,
            &mut self.hl,
            theme,
            false,
            focus,
            view,
            &mut self.synced,
        );
        let sel = out.sel.unwrap_or(doc.sel);
        if out.changed {
            doc.set_text(self.buf.clone(), sel);
            self.rev = Some(doc.rev);
        } else {
            doc.sel = sel;
        }
        self.synced = Some(doc.sel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;

    struct S {
        ed: SourceEditor,
        doc: Doc,
        theme: Theme,
    }

    #[test]
    fn source_view_drag_selects() {
        let s = S { ed: SourceEditor::default(), doc: Doc::new("Hello wonderful world".into(), None), theme: Theme::dark() };
        let mut h = Harness::builder().with_size(egui::Vec2::new(700.0, 300.0)).build_ui_state(
            |ui, s: &mut S| s.ed.ui(ui, &mut s.doc, &s.theme, &View { marks: &[], scroll_to: None, base_dir: None }),
            s,
        );
        let (from, to) = (egui::pos2(20.0, 16.0), egui::pos2(150.0, 16.0));
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
        let r = h.state().doc.sel.range();
        assert!(r.len() > 5, "drag should select text, got {r:?}");
    }
}
