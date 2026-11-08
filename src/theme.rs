use crate::fonts;
use crate::markdown::{Hl, Style};
use eframe::egui::{self, Color32, FontFamily, FontId, Stroke, TextFormat, Visuals};

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    pub bg: Color32,
    pub panel: Color32,
    pub text: Color32,
    pub faint: Color32,
    pub marker: Color32,
    pub accent: Color32,
    pub code_bg: Color32,
    pub code_text: Color32,
    pub border: Color32,
    pub quote_bar: Color32,
    pub selection: Color32,
    pub html: Color32,
    pub hover: Color32,
    pub find: Color32,
    pub find_current: Color32,
}

pub const BASE: f32 = 16.0;
pub const MONO: f32 = 14.0;
pub const COLUMN: f32 = 720.0;

impl Theme {
    pub fn dark() -> Self {
        Self {
            dark: true,
            bg: Color32::from_rgb(0x10, 0x10, 0x14),
            panel: Color32::from_rgb(0x14, 0x14, 0x19),
            text: Color32::from_rgb(0xE6, 0xE6, 0xEA),
            faint: Color32::from_rgb(0x8A, 0x8A, 0x96),
            marker: Color32::from_rgb(0x5E, 0x5E, 0x6E),
            accent: Color32::from_rgb(0x9B, 0x7C, 0xFF),
            code_bg: Color32::from_rgb(0x1C, 0x1C, 0x23),
            code_text: Color32::from_rgb(0xE0, 0xE0, 0xE6),
            border: Color32::from_rgb(0x2C, 0x2C, 0x36),
            quote_bar: Color32::from_rgb(0xD8, 0xD8, 0xE0),
            selection: Color32::from_rgba_unmultiplied(0x6E, 0x9C, 0xD8, 0x70),
            html: Color32::from_rgb(0xE0, 0x9B, 0x6B),
            hover: Color32::from_rgb(0x22, 0x22, 0x2B),
            find: Color32::from_rgba_unmultiplied(0xFA, 0xCC, 0x15, 0x50),
            find_current: Color32::from_rgba_unmultiplied(0xFB, 0x92, 0x3C, 0xB0),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            bg: Color32::from_rgb(0xFB, 0xFA, 0xFE),
            panel: Color32::from_rgb(0xF6, 0xF5, 0xFA),
            text: Color32::from_rgb(0x1F, 0x1F, 0x25),
            faint: Color32::from_rgb(0x6B, 0x6B, 0x78),
            marker: Color32::from_rgb(0xA8, 0xA8, 0xB4),
            accent: Color32::from_rgb(0x6A, 0x3D, 0xF0),
            code_bg: Color32::from_rgb(0xEF, 0xEE, 0xF3),
            code_text: Color32::from_rgb(0x2A, 0x2A, 0x32),
            border: Color32::from_rgb(0xDC, 0xDB, 0xE4),
            quote_bar: Color32::from_rgb(0x2A, 0x2A, 0x32),
            selection: Color32::from_rgba_unmultiplied(0x8C, 0xC4, 0xF0, 0x80),
            html: Color32::from_rgb(0xB0, 0x5A, 0x20),
            hover: Color32::from_rgb(0xEC, 0xEA, 0xF3),
            find: Color32::from_rgba_unmultiplied(0xFA, 0xCC, 0x15, 0x70),
            find_current: Color32::from_rgba_unmultiplied(0xF9, 0x73, 0x16, 0x90),
        }
    }

    pub fn apply(&self, ctx: &egui::Context) {
        let mut v = if self.dark { Visuals::dark() } else { Visuals::light() };
        v.panel_fill = self.bg;
        v.window_fill = self.panel;
        v.extreme_bg_color = self.bg;
        v.faint_bg_color = self.code_bg;
        v.override_text_color = Some(self.text);
        v.hyperlink_color = self.accent;
        v.selection.bg_fill = self.selection;
        v.selection.stroke = Stroke::new(1.0, self.text);
        v.text_cursor.stroke = Stroke::new(2.0, self.accent);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, self.border);
        v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        v.widgets.inactive.bg_stroke = Stroke::NONE;
        v.widgets.hovered.weak_bg_fill = self.hover;
        v.widgets.hovered.bg_stroke = Stroke::NONE;
        v.widgets.active.weak_bg_fill = self.border;
        v.widgets.active.bg_stroke = Stroke::NONE;
        v.widgets.open.weak_bg_fill = self.hover;
        for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
            w.fg_stroke.color = self.faint;
            w.corner_radius = 6.into();
        }
        v.widgets.hovered.fg_stroke.color = self.text;
        v.widgets.active.fg_stroke.color = self.text;
        ctx.set_visuals(v);
    }

    pub fn heading_size(level: u8) -> f32 {
        match level {
            1 => 30.0,
            2 => 24.0,
            3 => 20.0,
            4 => 18.0,
            5 => BASE,
            _ => 14.5,
        }
    }

    fn family(bold: bool, italic: bool) -> FontFamily {
        match (bold, italic) {
            (true, true) => fonts::bold_italic(),
            (true, false) => fonts::bold(),
            (false, true) => fonts::italic(),
            (false, false) => FontFamily::Proportional,
        }
    }

    /// Format for rendered (non-editing) text.
    pub fn rendered(&self, s: &Style, heading: u8, code_block: bool) -> TextFormat {
        let size = if heading > 0 { Self::heading_size(heading) } else { BASE };
        let mut f = TextFormat {
            font_id: FontId::new(size, Self::family(s.bold || heading > 0, s.italic)),
            color: self.text,
            line_height: Some(size * if heading > 0 { 1.3 } else { 1.6 }),
            ..Default::default()
        };
        if s.code || code_block {
            f.font_id = FontId::monospace(MONO);
            f.color = self.code_text;
            if !code_block {
                f.background = self.code_bg;
                f.expand_bg = 2.0;
            }
        }
        if s.html {
            f.color = self.html;
            f.font_id = FontId::monospace(MONO);
        }
        if let Some([r, g, b]) = s.color {
            f.color = Color32::from_rgb(r, g, b);
        }
        if s.link.is_some() {
            f.color = self.accent;
            f.underline = Stroke::new(1.0, self.accent);
        }
        if s.strike {
            f.strikethrough = Stroke::new(1.0, f.color);
        }
        f
    }

    /// Format for raw markdown being edited. `live` = proportional "inline
    /// editing" look; otherwise the monospace source view.
    pub fn source(&self, s: &Hl, live: bool) -> TextFormat {
        let mut f = TextFormat { color: self.text, ..Default::default() };

        if live {
            let size = if s.heading > 0 { Self::heading_size(s.heading) } else { BASE };
            f.font_id = FontId::new(size, Self::family(s.bold || s.heading > 0, s.italic));
            f.line_height = Some(size * if s.heading > 0 { 1.3 } else { 1.6 });
            if s.code || s.code_block || s.html {
                f.font_id = FontId::monospace(MONO);
                f.line_height = Some(MONO * 1.6);
                f.color = self.code_text;
            }
            if s.code && !s.code_block {
                f.background = self.code_bg;
            }
        } else {
            f.font_id = FontId::monospace(MONO);
            f.line_height = Some(MONO * 1.6);
            if s.heading > 0 {
                f.color = self.accent;
                f.font_id = FontId::new(MONO, FontFamily::Monospace);
            }
            if s.bold || s.italic {
                f.color = if self.dark { Color32::from_rgb(0xF5, 0xC2, 0x6B) } else { Color32::from_rgb(0x9A, 0x5B, 0x00) };
            }
            if s.code || s.code_block {
                f.color = if self.dark { Color32::from_rgb(0x8E, 0xD1, 0x9C) } else { Color32::from_rgb(0x1E, 0x7A, 0x3C) };
            }
        }

        if s.html {
            f.color = self.html;
        }
        if s.link {
            f.color = self.accent;
        }
        if s.quote && !s.marker {
            f.color = self.faint;
        }
        if s.strike {
            f.strikethrough = Stroke::new(1.0, f.color);
        }
        if s.marker {
            f.color = self.marker;
            f.strikethrough = Stroke::NONE;
        }
        f
    }
}
