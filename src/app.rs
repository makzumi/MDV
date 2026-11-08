use crate::doc::{Doc, Prefix};
use crate::editor::{SourceEditor, View};
use crate::live::{Format, LiveEditor};
use crate::find::Find;
use crate::images;
use crate::theme::{COLUMN, Theme};
use eframe::egui::{
    self, Align, Button, FontId, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, RichText, ScrollArea,
    ViewportCommand,
};
use egui_phosphor::regular as icon;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

const WELCOME: &str = include_str!("welcome.md");
const TOOLBAR_ROW: f32 = 46.0;

/// Block types offered in the heading menus.
pub const HEADINGS: [(u8, &str, &str); 7] = [
    (0, "Paragraph", icon::PARAGRAPH),
    (1, "Heading 1", icon::TEXT_H_ONE),
    (2, "Heading 2", icon::TEXT_H_TWO),
    (3, "Heading 3", icon::TEXT_H_THREE),
    (4, "Heading 4", icon::TEXT_H_FOUR),
    (5, "Heading 5", icon::TEXT_H_FIVE),
    (6, "Heading 6", icon::TEXT_H_SIX),
];

fn tb_btn(ui: &mut egui::Ui, glyph: &str, tip: &str, enabled: bool, act: &mut Option<Action>, a: Action) {
    let b = Button::new(RichText::new(glyph).size(17.0)).frame_when_inactive(false).min_size(egui::vec2(30.0, 30.0));
    if ui.add_enabled(enabled, b).on_hover_text(tip).clicked() {
        *act = Some(a);
    }
}

fn tb_sep(ui: &mut egui::Ui, color: egui::Color32) {
    ui.add_space(4.0);
    let r = ui.allocate_exact_size(egui::vec2(1.0, 20.0), egui::Sense::hover()).0;
    ui.painter().rect_filled(r, 0.0, color);
    ui.add_space(4.0);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Live,
    Source,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Undo,
    Redo,
    Heading(u8),
    Prefix(Prefix),
    CodeBlock,
    Wrap(&'static str),
    Link,
    Rule,
    ToggleMode,
    ToggleTheme,
    New,
    Open,
    Save,
    SaveAs,
    Find,
    Replace,
    FindNext,
    FindPrev,
    Image,
    Color(Option<&'static str>),
    ClearFormat,
}

/// Something that would throw away unsaved changes.
#[derive(Clone, PartialEq)]
enum Discard {
    New,
    Open,
    OpenPath(PathBuf),
    Close,
}

enum DialogKind {
    Open,
    SaveAs(Option<Discard>),
    Image,
}

enum DialogResult {
    Open(Option<PathBuf>),
    SaveAs(Option<PathBuf>, Option<Discard>),
    Image(Option<PathBuf>),
}

/// State of the "Insert link" dialog.
#[derive(Default)]
struct LinkDialog {
    url: String,
    /// Link text, when there's no selection to turn into a link.
    text: String,
    has_selection: bool,
    focus: bool,
}

/// State of the "Insert image" dialog.
#[derive(Default)]
struct ImageDialog {
    src: String,
    alt: String,
    focus: bool,
}

pub struct App {
    doc: Doc,
    mode: Mode,
    theme: Theme,
    live: LiveEditor,
    source: SourceEditor,
    title: String,
    confirm: Option<Discard>,
    dialog: Option<Receiver<DialogResult>>,
    allow_close: bool,
    status: Option<String>,
    find: Find,
    image_dialog: Option<ImageDialog>,
    link_dialog: Option<LinkDialog>,
    /// Toolbar measurements from the previous frame (for its responsive layout).
    tb_fmt_w: f32,
    tb_right_w: f32,
    tb_h: f32,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        crate::fonts::install(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let dark = cc.egui_ctx.system_theme().is_none_or(|t| t == egui::Theme::Dark);
        let theme = if dark { Theme::dark() } else { Theme::light() };
        theme.apply(&cc.egui_ctx);

        let mut app = Self {
            doc: Doc::new(WELCOME.to_owned(), None),
            mode: Mode::Live,
            theme,
            live: LiveEditor::default(),
            source: SourceEditor::default(),
            title: String::new(),
            confirm: None,
            dialog: None,
            allow_close: false,
            status: None,
            find: Find::default(),
            image_dialog: None,
            link_dialog: None,
            tb_fmt_w: 600.0,
            tb_right_w: 260.0,
            tb_h: TOOLBAR_ROW,
        };
        if let Some(p) = path {
            app.load(p);
        }
        // Debug builds only: start in a given state (used for screenshots).
        #[cfg(debug_assertions)]
        {
            // MDV_DEV_CARET=40 or MDV_DEV_CARET=20,200 (a selection).
            if let Ok(v) = std::env::var("MDV_DEV_CARET") {
                let mut it = v.split(',').filter_map(|x| x.trim().parse().ok());
                let a = it.next().unwrap_or(0);
                let b = it.next().unwrap_or(a);
                app.doc.sel = crate::doc::Sel { anchor: a, head: b };
                app.live.editing = true;
                app.live.focus_next = true;
            }
            if std::env::var("MDV_DEV_SOURCE").is_ok() {
                app.mode = Mode::Source;
            }
            if let Ok(q) = std::env::var("MDV_DEV_FIND") {
                app.find.open(&app.doc, true);
                app.find.query = q;
            }
            if std::env::var("MDV_DEV_LIGHT").is_ok() {
                app.theme = Theme::light();
                app.theme.apply(&cc.egui_ctx);
            }
        }
        app
    }

    fn load(&mut self, path: PathBuf) {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                self.doc = Doc::new(text, Some(path));
                self.find.close();
                self.live = LiveEditor::default();
                self.source = SourceEditor::default();
                self.status = None;
            }
            Err(e) => self.status = Some(format!("Could not open {}: {e}", path.display())),
        }
    }

    /// Returns `true` if the document was written.
    fn save_to(&mut self, path: PathBuf) -> bool {
        match std::fs::write(&path, &self.doc.text) {
            Ok(()) => {
                self.doc.path = Some(path);
                self.doc.dirty = false;
                self.status = Some("Saved".into());
                true
            }
            Err(e) => {
                self.status = Some(format!("Could not save: {e}"));
                false
            }
        }
    }

    fn spawn_dialog(&mut self, ctx: &egui::Context, kind: DialogKind) {
        if self.dialog.is_some() {
            return;
        }
        let (tx, rx) = channel();
        self.dialog = Some(rx);
        let ctx = ctx.clone();
        let dir = self.doc.path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
        let name = self.doc.path.as_ref().map_or("Untitled.md".to_owned(), |_| self.doc.title());
        std::thread::spawn(move || {
            let mut d = rfd::FileDialog::new();
            d = match kind {
                DialogKind::Image => d.add_filter("Images", &images::EXTENSIONS),
                _ => d.add_filter("Markdown", &["md", "markdown", "mdown", "mkd", "txt"]),
            };
            if let Some(dir) = dir {
                d = d.set_directory(dir);
            }
            let res = match kind {
                DialogKind::SaveAs(then) => DialogResult::SaveAs(d.set_file_name(name).save_file(), then),
                DialogKind::Open => DialogResult::Open(d.pick_file()),
                DialogKind::Image => DialogResult::Image(d.pick_file()),
            };
            let _ = tx.send(res);
            ctx.request_repaint();
        });
    }

    fn poll_dialog(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.dialog else { return };
        let Ok(res) = rx.try_recv() else { return };
        self.dialog = None;
        match res {
            DialogResult::Open(Some(p)) => self.load(p),
            DialogResult::SaveAs(Some(p), then) => {
                if self.save_to(p)
                    && let Some(d) = then
                {
                    self.discard(ctx, d);
                }
            }
            DialogResult::Image(Some(p)) => {
                let src = images::src_for(&p, self.base_dir().as_deref());
                if let Some(d) = &mut self.image_dialog {
                    d.src = src;
                } else {
                    self.insert_image(&src, "");
                }
            }
            _ => {}
        }
    }

    /// Run a discarding action, asking first if there are unsaved changes.
    fn request(&mut self, ctx: &egui::Context, d: Discard) {
        if self.doc.dirty {
            self.confirm = Some(d);
        } else {
            self.discard(ctx, d);
        }
    }

    fn discard(&mut self, ctx: &egui::Context, d: Discard) {
        match d {
            Discard::New => {
                self.doc = Doc::new(String::new(), None);
                self.live = LiveEditor::default();
                self.live.editing = true;
                self.live.focus_next = true;
                self.source = SourceEditor::default();
            }
            Discard::Open => self.spawn_dialog(ctx, DialogKind::Open),
            Discard::OpenPath(p) => self.load(p),
            Discard::Close => {
                self.allow_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    fn save(&mut self, ctx: &egui::Context, then: Option<Discard>) {
        match self.doc.path.clone() {
            Some(p) => {
                if self.save_to(p)
                    && let Some(d) = then
                {
                    self.discard(ctx, d);
                }
            }
            None => self.spawn_dialog(ctx, DialogKind::SaveAs(then)),
        }
    }

    fn base_dir(&self) -> Option<PathBuf> {
        self.doc.path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf())
    }

    fn insert_image(&mut self, src: &str, alt: &str) {
        self.doc.insert_block(&images::to_markdown(src, alt, None));
        self.live.editing = false;
    }

    fn apply(&mut self, ctx: &egui::Context, a: Action) {
        let edits = !matches!(
            a,
            Action::ToggleTheme
                | Action::New
                | Action::Open
                | Action::Save
                | Action::SaveAs
                | Action::Find
                | Action::Replace
                | Action::FindNext
                | Action::FindPrev
                | Action::Image
                | Action::Link
        );
        match a {
            Action::Undo => self.doc.undo(),
            Action::Redo => self.doc.redo(),
            Action::Heading(l) => self.doc.set_heading(l),
            Action::Prefix(p) => self.doc.toggle_prefix(p),
            Action::CodeBlock => self.doc.insert_code_block(),
            // In the editor, Ctrl+B with nothing selected formats what you type next.
            Action::Wrap(m) if self.mode == Mode::Live && self.doc.sel.is_empty() => self.live.set_pending(m),
            Action::Wrap(m) => self.doc.toggle_wrap(m),
            Action::Link => {
                self.link_dialog = Some(LinkDialog {
                    has_selection: !self.doc.sel.is_empty(),
                    focus: true,
                    ..Default::default()
                })
            }
            Action::Rule => self.doc.insert_rule(),
            Action::ToggleMode => {
                // The selection lives in `Doc`, so the caret carries across views.
                self.mode = if self.mode == Mode::Live { Mode::Source } else { Mode::Live };
                self.live.editing = false;
            }
            Action::ToggleTheme => {
                self.theme = if self.theme.dark { Theme::light() } else { Theme::dark() };
                self.theme.apply(ctx);
            }
            Action::New => self.request(ctx, Discard::New),
            Action::Open => self.request(ctx, Discard::Open),
            Action::Save => self.save(ctx, None),
            Action::SaveAs => self.spawn_dialog(ctx, DialogKind::SaveAs(None)),
            Action::Find => self.find.open(&self.doc, false),
            Action::Replace => self.find.open(&self.doc, true),
            Action::FindNext | Action::FindPrev => {
                if !self.find.open {
                    self.find.open(&self.doc, false);
                }
                self.find.step(&mut self.doc, a == Action::FindNext);
            }
            Action::Image => self.image_dialog = Some(ImageDialog { focus: true, ..Default::default() }),
            // With nothing selected in the editor, colors what you type next.
            Action::Color(c) if self.doc.sel.is_empty() => {
                if self.mode == Mode::Live {
                    self.live.set_pending_color(c);
                }
            }
            Action::Color(c) => self.doc.set_color(c),
            Action::ClearFormat if self.doc.sel.is_empty() => self.live.clear_pending(),
            Action::ClearFormat => self.doc.clear_formatting(),
        }
        if edits {
            // Keep (or put) the caret in an editor so the user can keep typing.
            match self.mode {
                Mode::Live => {
                    self.live.editing = true;
                    self.live.focus_next = true;
                }
                Mode::Source => self.source.focus_next = true,
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) -> Vec<Action> {
        let c = Modifiers::COMMAND;
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        // Order matters: Shift variants first (egui ignores extra Shift otherwise).
        let table: [(Modifiers, Key, Action); 25] = [
            (cs, Key::Z, Action::Redo),
            (c, Key::Z, Action::Undo),
            (c, Key::Y, Action::Redo),
            (cs, Key::S, Action::SaveAs),
            (c, Key::S, Action::Save),
            (c, Key::O, Action::Open),
            (c, Key::N, Action::New),
            (c, Key::B, Action::Wrap("**")),
            (c, Key::I, Action::Wrap("*")),
            (cs, Key::X, Action::Wrap("~~")),
            (c, Key::E, Action::Wrap("`")),
            (c, Key::K, Action::Link),
            (c, Key::Num0, Action::Heading(0)),
            (c, Key::Num1, Action::Heading(1)),
            (c, Key::Num2, Action::Heading(2)),
            (c, Key::Num3, Action::Heading(3)),
            (c, Key::Num4, Action::Heading(4)),
            (c, Key::Num5, Action::Heading(5)),
            (c, Key::Num6, Action::Heading(6)),
            (c, Key::Backslash, Action::ClearFormat),
            (c, Key::Slash, Action::ToggleMode),
            (c, Key::F, Action::Find),
            (c, Key::H, Action::Replace),
            (Modifiers::SHIFT, Key::F3, Action::FindPrev),
            (Modifiers::NONE, Key::F3, Action::FindNext),
        ];
        ctx.input_mut(|i| {
            table
                .iter()
                .filter(|(m, k, _)| i.consume_shortcut(&KeyboardShortcut::new(*m, *k)))
                .map(|(_, _, a)| *a)
                .collect()
        })
    }

    /// The main toolbar. Lays itself out for the available width: formatting
    /// centered with controls on the right; or both left-aligned; or, when
    /// narrow, formatting wrapped over several lines with controls below.
    fn toolbar(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut act = None;
        let avail = ui.available_width();
        let (fw, rw) = (self.tb_fmt_w, self.tb_right_w);
        let row = egui::vec2(avail, TOOLBAR_ROW);
        let top = ui.cursor().top();

        if avail >= fw + rw + 24.0 {
            let centered = avail >= fw + 2.0 * rw + 32.0;
            ui.allocate_ui_with_layout(row, Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let lead = if centered { ((avail - fw) / 2.0).max(0.0) } else { 0.0 };
                ui.add_space(lead);
                self.tb_fmt_w = self.fmt_group(ui, &mut act);
                ui.add_space((avail - lead - self.tb_fmt_w - rw - 4.0).max(0.0));
                self.tb_right_w = self.right_group(ui, &mut act);
            });
        } else {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(2.0, 4.0);
                self.fmt_group(ui, &mut act);
            });
            ui.allocate_ui_with_layout(egui::vec2(avail, 40.0), Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.add_space((avail - rw - 4.0).max(0.0));
                self.tb_right_w = self.right_group(ui, &mut act);
            });
        }

        let h = (ui.cursor().top() - top).max(TOOLBAR_ROW);
        if (h - self.tb_h).abs() > 0.5 {
            self.tb_h = h;
            ui.ctx().request_repaint();
        }
        act
    }

    /// Formatting buttons. Returns their total width.
    fn fmt_group(&self, ui: &mut egui::Ui, act: &mut Option<Action>) -> f32 {
        let t = self.theme;
        let x0 = ui.cursor().left();
        tb_btn(ui, icon::ARROW_U_UP_LEFT, "Undo (Ctrl+Z)", self.doc.can_undo(), act, Action::Undo);
        tb_btn(ui, icon::ARROW_U_UP_RIGHT, "Redo (Ctrl+Shift+Z)", self.doc.can_redo(), act, Action::Redo);
        tb_sep(ui, t.border);
        ui.menu_button(RichText::new(format!("{}{}", icon::TEXT_H, icon::CARET_DOWN)).size(17.0), |ui| {
            ui.set_min_width(150.0);
            for (lvl, name, glyph) in HEADINGS {
                if ui.button(format!("{glyph}  {name}")).clicked() {
                    *act = Some(Action::Heading(lvl));
                }
            }
        })
        .response
        .on_hover_text("Heading (Ctrl+0…6)");
        tb_btn(ui, icon::LIST_BULLETS, "Bullet list", true, act, Action::Prefix(Prefix::Bullet));
        tb_btn(ui, icon::LIST_NUMBERS, "Numbered list", true, act, Action::Prefix(Prefix::Number));
        tb_btn(ui, icon::LIST_CHECKS, "Task list", true, act, Action::Prefix(Prefix::Task));
        tb_sep(ui, t.border);
        tb_btn(ui, icon::QUOTES, "Quote", true, act, Action::Prefix(Prefix::Quote));
        tb_btn(ui, icon::CODE_BLOCK, "Code block", true, act, Action::CodeBlock);
        tb_btn(ui, icon::MINUS, "Divider", true, act, Action::Rule);
        tb_sep(ui, t.border);
        tb_btn(ui, icon::TEXT_B, "Bold (Ctrl+B)", true, act, Action::Wrap("**"));
        tb_btn(ui, icon::TEXT_ITALIC, "Italic (Ctrl+I)", true, act, Action::Wrap("*"));
        tb_btn(ui, icon::TEXT_STRIKETHROUGH, "Strikethrough (Ctrl+Shift+X)", true, act, Action::Wrap("~~"));
        tb_btn(ui, icon::CODE, "Inline code (Ctrl+E)", true, act, Action::Wrap("`"));
        tb_btn(ui, icon::LINK, "Link (Ctrl+K)", true, act, Action::Link);
        tb_btn(ui, icon::IMAGE, "Insert image", true, act, Action::Image);
        tb_sep(ui, t.border);
        ui.menu_button(RichText::new(icon::TEXT_A_UNDERLINE).size(17.0), |ui| {
            if let Some(c) = crate::live::color_picker(ui, &t, None) {
                *act = Some(Action::Color(c));
                ui.close();
            }
        })
        .response
        .on_hover_text("Text color");
        tb_btn(ui, icon::TEXT_T_SLASH, "Clear formatting (Ctrl+\\)", true, act, Action::ClearFormat);
        ui.cursor().left() - x0
    }

    /// File and view controls. Returns their total width.
    fn right_group(&self, ui: &mut egui::Ui, act: &mut Option<Action>) -> f32 {
        let t = self.theme;
        let x0 = ui.cursor().left();
        tb_btn(ui, icon::FILE_PLUS, "New (Ctrl+N)", true, act, Action::New);
        tb_btn(ui, icon::FOLDER_OPEN, "Open (Ctrl+O)", true, act, Action::Open);
        tb_btn(ui, icon::FLOPPY_DISK, "Save (Ctrl+S)", true, act, Action::Save);
        tb_sep(ui, t.border);
        let (glyph, label, tip) = match self.mode {
            Mode::Live => (icon::CODE, "Source", "View markdown source (Ctrl+/)"),
            Mode::Source => (icon::TEXT_AA, "Editor", "Back to the editor (Ctrl+/)"),
        };
        let toggle = Button::new(RichText::new(format!("{glyph}  {label}")).size(14.0))
            .selected(self.mode == Mode::Source)
            .min_size(egui::vec2(0.0, 30.0));
        if ui.add(toggle).on_hover_text(tip).clicked() {
            *act = Some(Action::ToggleMode);
        }
        tb_btn(ui, icon::MAGNIFYING_GLASS, "Find & replace (Ctrl+F / Ctrl+H)", true, act, Action::Find);
        let theme_icon = if t.dark { icon::SUN } else { icon::MOON };
        tb_btn(ui, theme_icon, "Toggle theme", true, act, Action::ToggleTheme);
        ui.cursor().left() - x0
    }

    fn image_dialog(&mut self, ctx: &egui::Context) {
        let Some(d) = &mut self.image_dialog else { return };
        let mut browse = false;
        let mut insert = false;
        let mut cancel = false;
        let faint = self.theme.faint;
        egui::Modal::new(egui::Id::new("insert-image")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new("Insert image").size(17.0).strong());
            ui.add_space(10.0);
            ui.label(RichText::new("Image URL or file").color(faint));
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut d.src).hint_text("https://… or path/to/image.png").desired_width(310.0),
                );
                if std::mem::take(&mut d.focus) {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    insert = true;
                }
                if ui.button(format!("{}  Browse…", icon::FOLDER_OPEN)).clicked() {
                    browse = true;
                }
            });
            ui.add_space(8.0);
            ui.label(RichText::new("Description (alt text)").color(faint));
            ui.add(egui::TextEdit::singleline(&mut d.alt).hint_text("Optional").desired_width(f32::INFINITY));
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add_enabled(!d.src.trim().is_empty(), Button::new("  Insert  ")).clicked() {
                    insert = true;
                }
                if ui.button("  Cancel  ").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    cancel = true;
                }
            });
        });
        if browse {
            self.spawn_dialog(ctx, DialogKind::Image);
        } else if insert && let Some(d) = self.image_dialog.take() {
            let src = d.src.trim().to_owned();
            if !src.is_empty() {
                self.insert_image(&src, d.alt.trim());
            }
        } else if cancel {
            self.image_dialog = None;
        }
    }

    fn link_dialog(&mut self, ctx: &egui::Context) {
        let Some(d) = &mut self.link_dialog else { return };
        let (mut insert, mut cancel) = (false, false);
        let faint = self.theme.faint;
        egui::Modal::new(egui::Id::new("insert-link")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new("Insert link").size(17.0).strong());
            ui.add_space(10.0);
            ui.label(RichText::new("URL").color(faint));
            let r = ui.add(egui::TextEdit::singleline(&mut d.url).hint_text("https://…").desired_width(f32::INFINITY));
            if std::mem::take(&mut d.focus) {
                r.request_focus();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                insert = true;
            }
            if !d.has_selection {
                ui.add_space(8.0);
                ui.label(RichText::new("Text").color(faint));
                ui.add(egui::TextEdit::singleline(&mut d.text).hint_text("Optional").desired_width(f32::INFINITY));
            }
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add_enabled(!d.url.trim().is_empty(), Button::new("  Insert  ")).clicked() {
                    insert = true;
                }
                if ui.button("  Cancel  ").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    cancel = true;
                }
            });
        });
        if insert && let Some(d) = self.link_dialog.take() {
            let url = d.url.trim();
            if !url.is_empty() {
                let url = if url.contains([' ', '(', ')']) { format!("<{url}>") } else { url.to_owned() };
                self.doc.wrap_link(&url, d.text.trim());
                self.live.focus_next = self.mode == Mode::Live;
                self.source.focus_next = self.mode == Mode::Source;
            }
        } else if cancel {
            self.link_dialog = None;
        }
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.confirm.clone() else { return };
        let name = self.doc.title();
        let mut choice = None;
        egui::Modal::new(egui::Id::new("confirm-discard")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.label(RichText::new(format!("Save changes to “{name}”?")).size(17.0).strong());
            ui.add_space(6.0);
            ui.label(RichText::new("Your changes will be lost if you don't save them.").color(self.theme.faint));
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("  Save  ").clicked() {
                    choice = Some(0);
                }
                if ui.button("  Don't save  ").clicked() {
                    choice = Some(1);
                }
                if ui.button("  Cancel  ").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    choice = Some(2);
                }
            });
        });
        match choice {
            Some(0) => {
                self.confirm = None;
                self.save(ctx, Some(pending));
            }
            Some(1) => {
                self.confirm = None;
                self.doc.dirty = false;
                self.discard(ctx, pending);
            }
            Some(_) => self.confirm = None,
            None => {}
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_dialog(&ctx);

        // Window close with unsaved changes → ask.
        if ctx.input(|i| i.viewport().close_requested()) && self.doc.dirty && !self.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.confirm = Some(Discard::Close);
        }
        // Drag & drop: images are inserted, anything else is opened.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        for p in dropped {
            if images::is_image_path(&p) {
                let src = images::src_for(&p, self.base_dir().as_deref());
                self.insert_image(&src, "");
            } else {
                self.request(&ctx, Discard::OpenPath(p));
                break;
            }
        }

        if self.confirm.is_none() && self.image_dialog.is_none() && self.link_dialog.is_none() {
            for a in self.shortcuts(&ctx) {
                self.apply(&ctx, a);
            }
        }

        let title = format!("{}{} — MDV", if self.doc.dirty { "• " } else { "" }, self.doc.title());
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        let t = self.theme;
        let toolbar_action = egui::Panel::top("toolbar")
            .exact_size(self.tb_h)
            .frame(Frame::NONE.fill(t.bg).inner_margin(Margin::symmetric(10, 0)))
            .show_separator_line(true)
            .show(ui, |ui| self.toolbar(ui))
            .inner;

        if self.find.open {
            egui::Panel::top("find")
                .exact_size(40.0)
                .frame(Frame::NONE.fill(t.panel).inner_margin(Margin::symmetric(10, 0)))
                .show_separator_line(true)
                .show(ui, |ui| {
                    if let Some(msg) = self.find.ui(ui, &mut self.doc, &t) {
                        self.status = Some(msg);
                    }
                });
        }
        self.find.refresh(&mut self.doc);
        let marks = self.find.marks();
        let scroll_to = std::mem::take(&mut self.find.scroll)
            .then(|| self.find.current.and_then(|i| self.find.matches.get(i)).map(|m| m.start))
            .flatten();
        let base_dir = self.base_dir();

        egui::Panel::bottom("status")
            .exact_size(26.0)
            .frame(Frame::NONE.fill(t.bg).inner_margin(Margin::symmetric(14, 0)))
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let small = |s: String| RichText::new(s).font(FontId::proportional(12.5)).color(t.faint);
                    let path = self.doc.path.as_ref().map_or("Unsaved document".into(), |p| p.display().to_string());
                    ui.label(small(self.status.clone().unwrap_or(path)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let words = self.doc.text.split_whitespace().count();
                        let mode = if self.mode == Mode::Live { "Editor" } else { "Source" };
                        ui.label(small(format!("{words} words  ·  {} chars  ·  {mode}", self.doc.text.chars().count())));
                    });
                });
            });

        egui::CentralPanel::default().frame(Frame::NONE.fill(t.bg)).show(ui, |ui| {
            ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                let avail = ui.available_width();
                let max = if self.mode == Mode::Live { COLUMN } else { COLUMN + 160.0 };
                let w = max.min(avail - 48.0).max(200.0);
                let margin = ((avail - w) / 2.0).max(0.0);
                ui.horizontal_top(|ui| {
                    ui.add_space(margin);
                    ui.vertical(|ui| {
                        ui.set_width(w);
                        ui.add_space(36.0);
                        let view = View { marks: &marks, scroll_to, base_dir: base_dir.as_deref() };
                        match self.mode {
                            Mode::Live => self.live.ui(ui, &mut self.doc, &t, &view),
                            Mode::Source => self.source.ui(ui, &mut self.doc, &t, &view),
                        }
                    });
                });
            });
        });

        if let Some(a) = toolbar_action {
            self.apply(&ctx, a);
        }
        // Floating selection toolbar in the editor.
        if let Some(f) = self.live.format.take() {
            let a = match f {
                Format::Heading(l) => Action::Heading(l),
                Format::Prefix(p) => Action::Prefix(p),
                Format::Wrap(m) => Action::Wrap(m),
                Format::Link => Action::Link,
                Format::Color(c) => Action::Color(c),
                Format::Clear => Action::ClearFormat,
            };
            self.apply(&ctx, a);
        }
        self.image_dialog(&ctx);
        self.link_dialog(&ctx);
        self.confirm_dialog(&ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    /// The whole app on the welcome document, "Features" heading selected
    /// (`sel` is relative to the start of the word) and the editor focused.
    fn app_at_features(sel: (usize, usize)) -> (Harness<'static, App>, usize) {
        let mut h = Harness::builder().with_size(egui::vec2(1100.0, 800.0)).build_eframe(|cc| App::new(cc, None));
        h.run();
        let a = h.state().doc.text.find("Features").unwrap();
        let s = h.state_mut();
        s.doc.sel = crate::doc::Sel { anchor: a + sel.0, head: a + sel.1 };
        s.live.focus_next = true;
        h.run();
        (h, a)
    }

    fn assert_heading_restored(h: &Harness<'static, App>, a: usize, how: &str) {
        let t = &h.state().doc.text;
        assert_eq!(&t[a - 5..a + 10], "\n\n## Features\n\n", "{how}: source");
        let rows = h.state().live.row_texts();
        assert!(rows.iter().any(|r| r == "Features"), "{how}: rendered rows {rows:?}");
        assert!(!rows.iter().any(|r| r.contains("##")), "{how}: markdown leaked into the view {rows:?}");
    }

    #[test]
    fn heading_to_text_and_back_via_floating_toolbar() {
        let (mut h, a) = app_at_features((0, 8));
        for (open, pick) in [("Heading 2  ", "Text"), ("Text  ", "Heading 2")] {
            h.get_by_label_contains(open).click();
            h.run();
            h.get_by_label_contains(pick).click();
            h.run();
        }
        assert_heading_restored(&h, a, "floating toolbar");
    }

    /// A human click: press and release in separate frames.
    fn slow_click(h: &mut Harness<'static, App>, label: &str) {
        // If the label is in both toolbars, take the floating one (lowest on screen).
        let p = h
            .get_all_by_label_contains(label)
            .map(|n| n.rect())
            .max_by(|a, b| a.top().total_cmp(&b.top()))
            .expect("button not found")
            .center();
        // Fixed frame counts: while the editor is focused the caret blink keeps
        // requesting frames, so `run()` would never settle.
        h.hover_at(p);
        h.run_steps(2);
        h.drag_at(p);
        // One frame held, like a quick human click. (`run()` would advance
        // simulated time past egui's click duration: the caret blink keeps
        // requesting frames.)
        h.step();
        h.event(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(3);
    }

    #[test]
    fn floating_toolbar_with_real_mouse_clicks() {
        // Heading → Text → Heading 2, pressing and releasing in separate frames.
        let (mut h, a) = app_at_features((0, 8));
        for (open, pick) in [("Heading 2  ", "Text"), ("Text  ", "Heading 2")] {
            slow_click(&mut h, open);
            slow_click(&mut h, pick);
        }
        assert_heading_restored(&h, a, "slow clicks");

        // The reported bug: after Text → Heading 2, Italic turned the line into
        // `*## Features*` (the selection had grown to include "## ").
        slow_click(&mut h, egui_phosphor::regular::TEXT_ITALIC);
        let t = &h.state().doc.text;
        assert_eq!(&t[a - 3..a + 10], "## *Features*", "italic after heading change keeps the heading");

        // The reported bug: italic on selected heading text.
        let (mut h, a) = app_at_features((0, 8));
        slow_click(&mut h, egui_phosphor::regular::TEXT_ITALIC);
        let t = &h.state().doc.text;
        assert_eq!(&t[a - 3..a + 10], "## *Features*", "italic keeps the heading");
        assert_eq!(&t[h.state().doc.sel.range()], "Features", "selection unchanged");
    }

    #[test]
    fn heading_to_text_and_back_via_menu_and_keys() {
        for (via_keys, sel) in [(false, (2, 2)), (false, (0, 8)), (true, (2, 2)), (true, (0, 8))] {
            let (mut h, a) = app_at_features(sel);
            for (lvl, key) in [(0u8, egui::Key::Num0), (2, egui::Key::Num2)] {
                if via_keys {
                    h.key_press_modifiers(egui::Modifiers::COMMAND, key);
                } else {
                    h.get_by_label(&format!("{}{}", icon::TEXT_H, icon::CARET_DOWN)).click();
                    h.run();
                    let name = if lvl == 0 { "Paragraph".to_owned() } else { format!("Heading {lvl}") };
                    h.get_by_label_contains(&name).click();
                }
                h.run();
            }
            assert_heading_restored(&h, a, &format!("keys={via_keys} sel={sel:?}"));
        }
    }
}
