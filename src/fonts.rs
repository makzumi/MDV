//! Font setup: use the desktop's own fonts (via fontconfig) so we ship no font
//! files and look native. Falls back to egui's built-in fonts if anything fails.

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use std::{process::Command, sync::Arc, thread};

pub const BOLD: &str = "mdv-bold";
pub const ITALIC: &str = "mdv-italic";
pub const BOLD_ITALIC: &str = "mdv-bold-italic";

pub fn bold() -> FontFamily {
    FontFamily::Name(BOLD.into())
}
pub fn italic() -> FontFamily {
    FontFamily::Name(ITALIC.into())
}
pub fn bold_italic() -> FontFamily {
    FontFamily::Name(BOLD_ITALIC.into())
}

/// Ask fontconfig which file backs `pattern`, then load it.
fn load(pattern: &str) -> Option<FontData> {
    let out = Command::new("fc-match")
        .args(["-f", "%{file}\n%{index}", pattern])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let mut lines = s.lines();
    let file = lines.next()?.trim().to_owned();
    let index = lines.next().and_then(|i| i.trim().parse().ok()).unwrap_or(0);

    // egui can only rasterize outline fonts.
    let lower = file.to_ascii_lowercase();
    if ![".ttf", ".otf", ".ttc", ".otc"].iter().any(|e| lower.ends_with(e)) {
        return None;
    }
    let mut data = FontData::from_owned(std::fs::read(&file).ok()?);
    data.index = index;
    Some(data)
}

pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(definitions(true));
}

/// Font setup. `system = false` skips fontconfig (deterministic, for tests).
pub fn definitions(system: bool) -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut defs, egui_phosphor::Variant::Regular);

    const PATTERNS: [(&str, &str); 5] = [
        ("sys-regular", "sans-serif:style=Regular"),
        ("sys-bold", "sans-serif:weight=bold"),
        ("sys-italic", "sans-serif:slant=italic"),
        ("sys-bold-italic", "sans-serif:weight=bold:slant=italic"),
        ("sys-mono", "monospace:style=Regular"),
    ];

    // fc-match is a process spawn each; do them concurrently to keep startup snappy.
    let loaded: Vec<(&str, Option<FontData>)> = thread::scope(|s| {
        let handles: Vec<_> = PATTERNS
            .iter()
            .map(|(name, pat)| (*name, s.spawn(move || if system { load(pat) } else { None })))
            .collect();
        handles
            .into_iter()
            .map(|(n, h)| (n, h.join().ok().flatten()))
            .collect()
    });

    let have = |name: &str| -> bool {
        loaded.iter().any(|(n, d)| *n == name && d.is_some())
    };
    let (has_reg, has_b, has_i, has_bi, has_mono) = (
        have("sys-regular"),
        have("sys-bold"),
        have("sys-italic"),
        have("sys-bold-italic"),
        have("sys-mono"),
    );

    for (name, data) in loaded {
        if let Some(d) = data {
            defs.font_data.insert(name.to_owned(), Arc::new(d));
        }
    }

    // Base fallback chain = egui defaults (+ phosphor icons, emoji).
    let fallback = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mono_fallback = defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();

    let chain = |primary: Option<&str>| -> Vec<String> {
        let mut v: Vec<String> = primary.map(|p| vec![p.to_owned()]).unwrap_or_default();
        if has_reg && primary != Some("sys-regular") {
            v.push("sys-regular".into());
        }
        v.extend(fallback.iter().cloned());
        v
    };

    defs.families.insert(FontFamily::Proportional, chain(has_reg.then_some("sys-regular")));
    defs.families.insert(bold(), chain(has_b.then_some("sys-bold")));
    defs.families.insert(italic(), chain(has_i.then_some("sys-italic")));
    defs.families.insert(
        bold_italic(),
        chain(if has_bi { Some("sys-bold-italic") } else if has_b { Some("sys-bold") } else { None }),
    );

    let mut mono: Vec<String> = if has_mono { vec!["sys-mono".to_owned()] } else { vec![] };
    mono.extend(mono_fallback);
    mono.push("phosphor".into());
    defs.families.insert(FontFamily::Monospace, mono);

    defs
}
