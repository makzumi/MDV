use crate::doc::{Doc, Prefix};
use crate::editor::{LiveEditor, SourceEditor, View};
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
        };
        if let Some(p) = path {
            app.load(p);
        }
        // Debug builds only: start in a given state (used for screenshots).
        #[cfg(debug_assertions)]
        {
            if let Some(at) = std::env::var("MDV_DEV_CARET").ok().and_then(|v| v.parse().ok()) {
                app.doc.sel = crate::doc::Sel::caret(at);
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
        );
        match a {
            Action::Undo => self.doc.undo(),
            Action::Redo => self.doc.redo(),
            Action::Heading(l) => self.doc.set_heading(l),
            Action::Prefix(p) => self.doc.toggle_prefix(p),
            Action::CodeBlock => self.doc.insert_code_block(),
            Action::Wrap(m) => self.doc.toggle_wrap(m),
            Action::Link => self.doc.insert_link(),
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
        let table: [(Modifiers, Key, Action); 21] = [
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

    fn toolbar(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut act = None;
        let t = self.theme;
        let btn = |ui: &mut egui::Ui, glyph: &str, tip: &str, enabled: bool, act: &mut Option<Action>, a: Action| {
            let b = Button::new(RichText::new(glyph).size(17.0)).frame_when_inactive(false).min_size(egui::vec2(30.0, 30.0));
            if ui.add_enabled(enabled, b).on_hover_text(tip).clicked() {
                *act = Some(a);
            }
        };
        let sep = |ui: &mut egui::Ui| {
            ui.add_space(4.0);
            let r = ui.allocate_exact_size(egui::vec2(1.0, 20.0), egui::Sense::hover()).0;
            ui.painter().rect_filled(r, 0.0, t.border);
            ui.add_space(4.0);
        };

        // Formatting group, centered (fixed button widths make it measurable).
        let group_w = 16.0 * 32.0 + 5.0 * 11.0;
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let full = ui.available_width();
            ui.add_space(((full - group_w) / 2.0).max(0.0));

            btn(ui, icon::ARROW_U_UP_LEFT, "Undo (Ctrl+Z)", self.doc.can_undo(), &mut act, Action::Undo);
            btn(ui, icon::ARROW_U_UP_RIGHT, "Redo (Ctrl+Shift+Z)", self.doc.can_redo(), &mut act, Action::Redo);
            sep(ui);
            ui.menu_button(RichText::new(format!("{}{}", icon::TEXT_H, icon::CARET_DOWN)).size(17.0), |ui| {
                ui.set_min_width(150.0);
                for (lvl, name, glyph) in [
                    (0u8, "Paragraph", icon::PARAGRAPH),
                    (1, "Heading 1", icon::TEXT_H_ONE),
                    (2, "Heading 2", icon::TEXT_H_TWO),
                    (3, "Heading 3", icon::TEXT_H_THREE),
                ] {
                    if ui.button(format!("{glyph}  {name}")).clicked() {
                        act = Some(Action::Heading(lvl));
                    }
                }
            })
            .response
            .on_hover_text("Heading (Ctrl+0…3)");
            btn(ui, icon::LIST_BULLETS, "Bullet list", true, &mut act, Action::Prefix(Prefix::Bullet));
            btn(ui, icon::LIST_NUMBERS, "Numbered list", true, &mut act, Action::Prefix(Prefix::Number));
            btn(ui, icon::LIST_CHECKS, "Task list", true, &mut act, Action::Prefix(Prefix::Task));
            sep(ui);
            btn(ui, icon::QUOTES, "Quote", true, &mut act, Action::Prefix(Prefix::Quote));
            btn(ui, icon::CODE_BLOCK, "Code block", true, &mut act, Action::CodeBlock);
            btn(ui, icon::MINUS, "Divider", true, &mut act, Action::Rule);
            sep(ui);
            btn(ui, icon::TEXT_B, "Bold (Ctrl+B)", true, &mut act, Action::Wrap("**"));
            btn(ui, icon::TEXT_ITALIC, "Italic (Ctrl+I)", true, &mut act, Action::Wrap("*"));
            btn(ui, icon::TEXT_STRIKETHROUGH, "Strikethrough (Ctrl+Shift+X)", true, &mut act, Action::Wrap("~~"));
            btn(ui, icon::CODE, "Inline code (Ctrl+E)", true, &mut act, Action::Wrap("`"));
            btn(ui, icon::LINK, "Link (Ctrl+K)", true, &mut act, Action::Link);
            btn(ui, icon::IMAGE, "Insert image", true, &mut act, Action::Image);

            // Right-aligned: file + view controls.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let theme_icon = if t.dark { icon::SUN } else { icon::MOON };
                btn(ui, theme_icon, "Toggle theme", true, &mut act, Action::ToggleTheme);
                btn(ui, icon::MAGNIFYING_GLASS, "Find & replace (Ctrl+F / Ctrl+H)", true, &mut act, Action::Find);

                let (glyph, label, tip) = match self.mode {
                    Mode::Live => (icon::CODE, "Source", "View markdown source (Ctrl+/)"),
                    Mode::Source => (icon::TEXT_AA, "Editor", "Back to the editor (Ctrl+/)"),
                };
                let toggle = Button::new(RichText::new(format!("{glyph}  {label}")).size(14.0))
                    .selected(self.mode == Mode::Source)
                    .min_size(egui::vec2(0.0, 30.0));
                if ui.add(toggle).on_hover_text(tip).clicked() {
                    act = Some(Action::ToggleMode);
                }
                sep(ui);
                btn(ui, icon::FLOPPY_DISK, "Save (Ctrl+S)", true, &mut act, Action::Save);
                btn(ui, icon::FOLDER_OPEN, "Open (Ctrl+O)", true, &mut act, Action::Open);
                btn(ui, icon::FILE_PLUS, "New (Ctrl+N)", true, &mut act, Action::New);
            });
        });
        act
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

        if self.confirm.is_none() && self.image_dialog.is_none() {
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
            .exact_size(46.0)
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
        self.image_dialog(&ctx);
        self.confirm_dialog(&ctx);
    }
}
