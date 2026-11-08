//! Image references: parsing `<img>` tags, resolving paths to loadable URIs,
//! and writing image markdown back out.

use std::ops::Range;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Width {
    Px(f32),
    Percent(f32),
}

impl Width {
    fn parse(v: &str) -> Option<Self> {
        let v = v.trim();
        if let Some(p) = v.strip_suffix('%') {
            p.trim().parse().ok().map(Width::Percent)
        } else {
            v.trim_end_matches("px").trim().parse().ok().map(Width::Px)
        }
    }

    /// Resolve against the available column width.
    pub fn resolve(self, column: f32) -> f32 {
        match self {
            Width::Px(p) => p,
            Width::Percent(p) => column * p / 100.0,
        }
    }
}

/// An image in the document, from `![alt](src)` or `<img src=… alt=… width=…>`.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageRef {
    pub src: String,
    pub alt: String,
    pub width: Option<Width>,
    /// Byte range of the whole image syntax in the source.
    pub range: Range<usize>,
}

pub const EXTENSIONS: [&str; 8] = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico"];

pub fn is_image_path(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// A parsed `<img …>` tag: (byte range within the input, src, alt, width).
pub struct ImgTag {
    pub range: Range<usize>,
    pub src: String,
    pub alt: String,
    pub width: Option<Width>,
}

/// Find all `<img …>` tags in a chunk of HTML.
pub fn find_img_tags(html: &str) -> Vec<ImgTag> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(off) = lower[from..].find("<img") {
        let start = from + off;
        let after = start + 4;
        // Must be followed by whitespace, `/` or `>` (not e.g. `<imgfoo`).
        if !lower[after..].starts_with(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>') {
            from = after;
            continue;
        }
        let Some(end) = tag_end(html, after) else { break };
        // Drop the self-closing `/` so `width=50%/>` doesn't read as "50%/".
        let inner = html[after..end - 1].trim_end();
        let attrs = parse_attrs(inner.strip_suffix('/').unwrap_or(inner));
        let get = |k: &str| attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        if let Some(src) = get("src") {
            out.push(ImgTag {
                range: start..end,
                src,
                alt: get("alt").unwrap_or_default(),
                width: get("width").and_then(|w| Width::parse(&w)),
            });
        }
        from = end;
    }
    out
}

/// Index just past the `>` closing a tag, honouring quoted attribute values.
fn tag_end(s: &str, from: usize) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in s[from..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return Some(from + i + 1),
            _ => {}
        }
    }
    None
}

fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut it = s.char_indices().peekable();
    while let Some(&(i, c)) = it.peek() {
        if c.is_ascii_whitespace() || c == '/' {
            it.next();
            continue;
        }
        // name
        let mut end = i;
        while let Some(&(j, c)) = it.peek() {
            if c == '=' || c.is_ascii_whitespace() || c == '/' {
                break;
            }
            end = j + c.len_utf8();
            it.next();
        }
        let name = s[i..end].to_ascii_lowercase();
        while it.peek().is_some_and(|&(_, c)| c.is_ascii_whitespace()) {
            it.next();
        }
        let mut value = String::new();
        if it.peek().is_some_and(|&(_, c)| c == '=') {
            it.next();
            while it.peek().is_some_and(|&(_, c)| c.is_ascii_whitespace()) {
                it.next();
            }
            match it.peek().map(|&(_, c)| c) {
                Some(q @ ('"' | '\'')) => {
                    it.next();
                    for (_, c) in it.by_ref() {
                        if c == q {
                            break;
                        }
                        value.push(c);
                    }
                }
                _ => {
                    while let Some(&(_, c)) = it.peek() {
                        if c.is_ascii_whitespace() {
                            break;
                        }
                        value.push(c);
                        it.next();
                    }
                }
            }
        }
        if !name.is_empty() {
            out.push((name, decode_entities(&value)));
        }
    }
    out
}

pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// Visible words of an HTML fragment, with their byte ranges (tags skipped,
/// entities decoded).
pub fn visible_words(html: &str) -> Vec<(Range<usize>, String)> {
    let mut out = Vec::new();
    let mut word_start: Option<usize> = None;
    let mut i = 0;
    let flush = |out: &mut Vec<(Range<usize>, String)>, ws: &mut Option<usize>, end: usize| {
        if let Some(st) = ws.take() {
            out.push((st..end, decode_entities(&html[st..end])));
        }
    };
    while i < html.len() {
        if html[i..].starts_with('<')
            && let Some(end) = tag_end(html, i + 1)
        {
            flush(&mut out, &mut word_start, i);
            i = end;
            continue;
        }
        let c = html[i..].chars().next().unwrap();
        if c.is_whitespace() {
            flush(&mut out, &mut word_start, i);
        } else if word_start.is_none() {
            word_start = Some(i);
        }
        i += c.len_utf8();
    }
    flush(&mut out, &mut word_start, html.len());
    out
}

/// Hints from an HTML block's markup: heading level (`<h1>`…`<h6>`) and
/// whether it asks to be centered (`align="center"`, `text-align: center`).
pub fn block_hints(html: &str) -> (u8, bool) {
    let l = html.to_ascii_lowercase();
    let t = l.trim_start();
    let heading = match t.as_bytes() {
        [b'<', b'h', d @ b'1'..=b'6', ..] => d - b'0',
        _ => 0,
    };
    let compact: String = l.chars().filter(|c| !c.is_whitespace() && *c != '"' && *c != '\'').collect();
    let center = compact.contains("align=center") || compact.contains("text-align:center");
    (heading, center)
}

/// Turn a markdown image `src` into a URI egui's loaders understand.
pub fn resolve_uri(src: &str, base_dir: Option<&Path>) -> String {
    let lower = src.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("file://") {
        return src.to_owned();
    }
    let decoded = percent_decode(src);
    let p = Path::new(&decoded);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        match base_dir {
            Some(b) => b.join(p),
            None => std::env::current_dir().unwrap_or_default().join(p),
        }
    };
    format!("file://{}", abs.display())
}

fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}

/// Markdown for an image. Plain `![alt](src)` when there's no size; an
/// `<img>` tag (which GitHub and most renderers honour) when there is one.
pub fn to_markdown(src: &str, alt: &str, width: Option<Width>) -> String {
    match width {
        None => {
            let alt = alt.replace('[', "\\[").replace(']', "\\]");
            if src.contains([' ', '(', ')', '<', '>']) {
                format!("![{alt}](<{src}>)")
            } else {
                format!("![{alt}]({src})")
            }
        }
        Some(w) => {
            let attr = |v: &str| v.replace('&', "&amp;").replace('"', "&quot;");
            let w = match w {
                Width::Px(p) => format!("{}", p.round() as i64),
                Width::Percent(p) => format!("{}%", p.round() as i64),
            };
            format!("<img src=\"{}\" alt=\"{}\" width=\"{w}\">", attr(src), attr(alt))
        }
    }
}

/// How to refer to `image` from a document in `doc_dir`: relative if it's
/// inside that folder, absolute otherwise.
pub fn src_for(image: &Path, doc_dir: Option<&Path>) -> String {
    if let Some(dir) = doc_dir
        && let Ok(rel) = image.strip_prefix(dir)
    {
        return rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
    }
    image.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_img_tags() {
        let html = r#"<p><img src="a b.png" alt='Logo' width="96"> and <IMG SRC=x.jpg width=50%/></p>"#;
        let tags = find_img_tags(html);
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].src, "a b.png");
        assert_eq!(tags[0].alt, "Logo");
        assert_eq!(tags[0].width, Some(Width::Px(96.0)));
        assert_eq!(&html[tags[0].range.clone()], r#"<img src="a b.png" alt='Logo' width="96">"#);
        assert_eq!(tags[1].src, "x.jpg");
        assert_eq!(tags[1].width, Some(Width::Percent(50.0)));
    }

    #[test]
    fn markdown_roundtrip() {
        assert_eq!(to_markdown("a.png", "A", None), "![A](a.png)");
        assert_eq!(to_markdown("my pic.png", "", None), "![](<my pic.png>)");
        assert_eq!(to_markdown("a.png", "say \"hi\"", Some(Width::Px(320.4))), r#"<img src="a.png" alt="say &quot;hi&quot;" width="320">"#);
        let tags = find_img_tags(&to_markdown("a.png", "say \"hi\"", Some(Width::Px(10.0))));
        assert_eq!(tags[0].alt, "say \"hi\"");
    }

    #[test]
    fn resolves_paths() {
        let base = Path::new("/home/u/docs");
        assert_eq!(resolve_uri("img/a%20b.png", Some(base)), "file:///home/u/docs/img/a b.png");
        assert_eq!(resolve_uri("https://x.org/a.png", Some(base)), "https://x.org/a.png");
        assert_eq!(src_for(Path::new("/home/u/docs/img/a.png"), Some(base)), "img/a.png");
        assert_eq!(src_for(Path::new("/tmp/a.png"), Some(base)), "/tmp/a.png");
    }

    #[test]
    fn words_and_hints() {
        let html = "  A <b>bold</b> &amp; text<br>";
        let w = visible_words(html);
        let words: Vec<&str> = w.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(words, ["A", "bold", "&", "text"]);
        assert_eq!(&html[w[1].0.clone()], "bold");
        assert_eq!(block_hints("<h1 align=\"center\">MDV</h1>"), (1, true));
        assert_eq!(block_hints("<p style=\"text-align: center\">x</p>"), (0, true));
        assert_eq!(block_hints("<p>x</p>"), (0, false));
    }
}
