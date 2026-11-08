//! Find & replace over the markdown source (plain text, optional match-case).

use crate::doc::{Doc, Sel};
use crate::theme::Theme;
use eframe::egui::{self, Button, Id, Key, RichText, TextEdit};
use egui_phosphor::regular as icon;
use std::ops::Range;

const MAX_MATCHES: usize = 10_000;
pub const FIND_ID: &str = "mdv-find";

/// Byte ranges of every occurrence of `q` in `text`.
pub fn search(text: &str, q: &str, match_case: bool) -> Vec<Range<usize>> {
    if q.is_empty() {
        return vec![];
    }
    let to_ranges = |it: std::str::MatchIndices<'_, &str>| it.map(|(i, m)| i..i + m.len()).take(MAX_MATCHES).collect();
    if match_case {
        return to_ranges(text.match_indices(q));
    }
    if q.is_ascii() {
        // ASCII lowercasing keeps byte offsets identical to the original.
        let t = text.to_ascii_lowercase();
        let ql = q.to_ascii_lowercase();
        return to_ranges(t.match_indices(ql.as_str()));
    }
    // General Unicode case-insensitive scan.
    let ql: Vec<char> = q.chars().flat_map(char::to_lowercase).collect();
    let mut out = Vec::new();
    for (i, _) in text.char_indices() {
        let mut want = ql.iter();
        let mut end = i;
        let mut ok = true;
        'chars: for c in text[i..].chars() {
            for lc in c.to_lowercase() {
                match want.next() {
                    Some(w) if *w == lc => {}
                    Some(_) => {
                        ok = false;
                        break 'chars;
                    }
                    None => break 'chars,
                }
            }
            end += c.len_utf8();
            if want.len() == 0 {
                break;
            }
        }
        if ok && want.len() == 0 {
            out.push(i..end);
            if out.len() >= MAX_MATCHES {
                break;
            }
        }
    }
    out
}

#[derive(Default)]
pub struct Find {
    pub open: bool,
    pub show_replace: bool,
    pub query: String,
    pub replacement: String,
    pub match_case: bool,
    pub matches: Vec<Range<usize>>,
    pub current: Option<usize>,
    /// Ask the editor to scroll the current match into view (one-shot).
    pub scroll: bool,
    focus: bool,
    key: Option<(u64, String, bool)>,
}

impl Find {
    pub fn open(&mut self, doc: &Doc, replace: bool) {
        let sel = &doc.text[doc.sel.range()];
        if !sel.is_empty() && !sel.contains('\n') {
            self.query = sel.to_owned();
        }
        self.open = true;
        self.show_replace |= replace;
        self.focus = true;
        self.key = None;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.matches.clear();
        self.current = None;
        self.key = None;
    }

    /// Recompute matches if the text or query changed.
    pub fn refresh(&mut self, doc: &mut Doc) {
        if !self.open {
            return;
        }
        let key = (doc.rev, self.query.clone(), self.match_case);
        if self.key.as_ref() == Some(&key) {
            return;
        }
        let query_changed = self.key.as_ref().is_none_or(|k| k.1 != key.1 || k.2 != key.2);
        self.key = Some(key);
        self.matches = search(&doc.text, &self.query, self.match_case);
        let from = doc.sel.range().start;
        self.current = self.matches.iter().position(|m| m.start >= from).or((!self.matches.is_empty()).then_some(0));
        if query_changed && self.current.is_some() {
            self.select(doc);
        }
    }

    fn select(&mut self, doc: &mut Doc) {
        if let Some(m) = self.current.and_then(|i| self.matches.get(i)) {
            doc.sel = Sel { anchor: m.start, head: m.end };
            self.scroll = true;
        }
    }

    pub fn step(&mut self, doc: &mut Doc, forward: bool) {
        self.refresh(doc);
        let n = self.matches.len();
        if n == 0 {
            return;
        }
        self.current = Some(match self.current {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None => 0,
        });
        self.select(doc);
    }

    pub fn replace_current(&mut self, doc: &mut Doc) {
        self.refresh(doc);
        let Some(m) = self.current.and_then(|i| self.matches.get(i)).cloned() else { return };
        let after = m.start + self.replacement.len();
        doc.replace(m, &self.replacement, Sel::caret(after), false);
        self.refresh(doc);
        // Continue with the next match after the replaced text.
        self.current = self.matches.iter().position(|m| m.start >= after).or((!self.matches.is_empty()).then_some(0));
        self.select(doc);
    }

    pub fn replace_all(&mut self, doc: &mut Doc) -> usize {
        self.refresh(doc);
        if self.matches.is_empty() {
            return 0;
        }
        let mut out = String::with_capacity(doc.text.len());
        let mut last = 0;
        for m in &self.matches {
            out.push_str(&doc.text[last..m.start]);
            out.push_str(&self.replacement);
            last = m.end;
        }
        out.push_str(&doc.text[last..]);
        let n = self.matches.len();
        let len = doc.text.len();
        doc.replace(0..len, &out, Sel::caret(0), false);
        self.refresh(doc);
        n
    }

    /// Ranges to highlight: (range, is_current). Empty when closed.
    pub fn marks(&self) -> Vec<(Range<usize>, bool)> {
        if !self.open {
            return vec![];
        }
        self.matches.iter().enumerate().map(|(i, m)| (m.clone(), Some(i) == self.current)).collect()
    }

    /// The find bar. Returns a status message (e.g. after "Replace all").
    pub fn ui(&mut self, ui: &mut egui::Ui, doc: &mut Doc, theme: &Theme) -> Option<String> {
        self.refresh(doc);
        let mut status = None;
        let small_btn = |glyph: &str| Button::new(RichText::new(glyph).size(16.0)).frame_when_inactive(false).min_size(egui::vec2(28.0, 28.0));

        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let toggle = if self.show_replace { icon::CARET_DOWN } else { icon::CARET_RIGHT };
            if ui.add(small_btn(toggle)).on_hover_text("Toggle replace (Ctrl+H)").clicked() {
                self.show_replace = !self.show_replace;
            }

            let find_resp = ui.add(
                TextEdit::singleline(&mut self.query)
                    .id(Id::new(FIND_ID))
                    .hint_text(format!("{}  Find", icon::MAGNIFYING_GLASS))
                    .desired_width(220.0),
            );
            if std::mem::take(&mut self.focus) {
                find_resp.request_focus();
                if let Some(mut st) = TextEdit::load_state(ui.ctx(), find_resp.id) {
                    let n = self.query.chars().count();
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(n),
                    )));
                    st.store(ui.ctx(), find_resp.id);
                }
            }
            if find_resp.lost_focus() {
                let (enter, shift, esc) = ui.input(|i| (i.key_pressed(Key::Enter), i.modifiers.shift, i.key_pressed(Key::Escape)));
                if esc {
                    self.close();
                    return;
                }
                if enter {
                    self.step(doc, !shift);
                    find_resp.request_focus();
                }
            }
            if find_resp.changed() {
                self.refresh(doc);
            }

            let count = match (self.matches.len(), self.current) {
                (0, _) if self.query.is_empty() => String::new(),
                (0, _) => "No results".into(),
                (n, Some(i)) => format!("{} of {n}", i + 1),
                (n, None) => format!("{n} found"),
            };
            ui.add_sized([74.0, 20.0], egui::Label::new(RichText::new(count).size(12.5).color(theme.faint)));

            if ui.add(small_btn(icon::ARROW_UP)).on_hover_text("Previous (Shift+Enter)").clicked() {
                self.step(doc, false);
            }
            if ui.add(small_btn(icon::ARROW_DOWN)).on_hover_text("Next (Enter / F3)").clicked() {
                self.step(doc, true);
            }
            let case = Button::new(RichText::new("Aa").size(14.0)).selected(self.match_case).min_size(egui::vec2(28.0, 28.0));
            if ui.add(case).on_hover_text("Match case").clicked() {
                self.match_case = !self.match_case;
            }

            if self.show_replace {
                ui.add_space(8.0);
                let rep = ui.add(TextEdit::singleline(&mut self.replacement).hint_text("Replace").desired_width(180.0));
                if rep.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    self.replace_current(doc);
                    rep.request_focus();
                }
                if ui.button("Replace").clicked() {
                    self.replace_current(doc);
                }
                if ui.button("All").on_hover_text("Replace all").clicked() {
                    let n = self.replace_all(doc);
                    status = Some(format!("Replaced {n} occurrence{}", if n == 1 { "" } else { "s" }));
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(small_btn(icon::X)).on_hover_text("Close (Esc)").clicked() {
                    self.close();
                }
            });
        });
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_modes() {
        let t = "Foo foo FOO";
        assert_eq!(search(t, "foo", true), vec![4..7]);
        assert_eq!(search(t, "foo", false).len(), 3);
        assert_eq!(search("Ärger ärger", "ärger", false), vec![0..6, 7..13]);
        assert!(search(t, "", false).is_empty());
    }

    #[test]
    fn replace_one_and_all() {
        let mut d = Doc::new("a cat, a Cat, a dog".into(), None);
        let mut f = Find::default();
        f.open(&d, true);
        f.query = "cat".into();
        f.replacement = "fox".into();
        f.refresh(&mut d);
        assert_eq!(f.matches.len(), 2);
        f.replace_current(&mut d);
        assert_eq!(d.text, "a fox, a Cat, a dog");
        assert_eq!(f.replace_all(&mut d), 1);
        assert_eq!(d.text, "a fox, a fox, a dog");
        d.undo();
        assert_eq!(d.text, "a fox, a Cat, a dog");
    }
}
