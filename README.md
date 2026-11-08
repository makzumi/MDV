<p align="center">
  <img src="assets/mdv.png" width="96" alt="MDV icon">
</p>

<h1 align="center">MDV</h1>

<p align="center">
  A light, fast, native markdown viewer and editor written in Rust.<br>
  No webview, no browser engine: just your text, drawn on the GPU.
</p>

<p align="center">
  <a href="https://github.com/makzumi/MDV/releases/latest"><img src="https://img.shields.io/github/v/release/makzumi/MDV?include_prereleases&label=download" alt="Latest release"></a>
  <a href="https://github.com/makzumi/MDV/actions/workflows/ci.yml"><img src="https://github.com/makzumi/MDV/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
</p>

<p align="center">
  <img src="assets/screenshots/ss1.png" alt="MDV in dark mode" width="49%">
  <img src="assets/screenshots/ss2.png" alt="MDV in light mode" width="49%">
</p>

## Features

- **Edit like a normal document.** The editor shows formatted text only, never markdown syntax. Format with the toolbar or shortcuts, and select text across paragraphs, lists and headings. Typing `- `, `1. `, `# ` or `> ` at the start of a line turns it into a list, heading or quote.
- **Floating toolbar.** Select text and a small toolbar appears above it. It has a block-type menu (text, headings 1–6, lists, quote) plus bold, italic, strikethrough, code, link, text color and clear formatting.
- **Text color and clear formatting.** Pick from 8 colors that read well in both themes, or strip all inline formatting from a selection in one click (links are kept).
- **Source view.** One click (or `Ctrl+/`) shows the raw markdown, syntax-highlighted, for anyone who'd rather write markdown directly. The caret stays where it was.
- **Your file stays yours.** MDV only stores the markdown text, so it never reformats or rewrites what you wrote.
- **Smart editing.** Lists and task lists continue on Enter, checkboxes toggle with a click, and there are toolbar buttons and shortcuts for all common formatting.
- **Images.** Local files (relative to the document) and web images, including PNG, JPEG, GIF, WebP and SVG. Insert one from the toolbar (paste a link or browse), or drop an image file onto the window. Click an image to select it, then drag a corner handle to resize it.
- **Find and replace.** `Ctrl+F` / `Ctrl+H`, with match-case, replace one or replace all. Matches are highlighted in both views.
- **Light.** Built with [egui](https://github.com/emilk/egui) on OpenGL. It only redraws when you do something, so it uses 0% CPU when idle. The binary is about 14 MB.
- **Native feel.** Uses your desktop's fonts and native file dialogs, with dark and light themes. The toolbar rearranges itself to fit narrow windows.

Linux first (Wayland and X11). Windows and macOS builds are produced too; there, MDV uses its built-in font instead of the system one (font discovery uses `fc-match`, see `src/fonts.rs`).

## Download

Grab the latest build from the [Releases page](https://github.com/makzumi/MDV/releases/latest):

| Platform | File |
|---|---|
| Linux | `MDV-<version>-x86_64.AppImage`: `chmod +x` it and run |
| Windows | `MDV-<version>-windows-x86_64.exe`: portable, no installer |
| macOS | `MDV-<version>-macos-universal.dmg`: Apple Silicon and Intel |

Builds are **not code-signed**, so on first launch:
- **macOS:** right-click the app → Open (or run `xattr -dr com.apple.quarantine /Applications/MDV.app`).
- **Windows:** SmartScreen → "More info" → "Run anyway".

## Build from source

Requires Rust 1.95+.

```sh
git clone https://github.com/makzumi/MDV.git
cd MDV
cargo run --release -- path/to/file.md
```

### Install on Linux (from source)

```sh
./packaging/linux/install.sh              # binary, desktop entry, icons → ~/.local
./packaging/linux/install.sh --uninstall
```

This adds MDV to your app launcher and to "Open with" for `.md` files. It is also what gives the window its icon on **Wayland**: the compositor finds the icon through the installed `mdv.desktop` file (it matches the window's app id `mdv`). Without it, you get the generic Wayland icon. AppImage users get the same effect by integrating the AppImage (for example with Gear Lever or AppImageLauncher).

## Making a release (maintainers)

Pushing a version tag builds everything on GitHub Actions ([.github/workflows/release.yml](.github/workflows/release.yml)) and creates a **draft** release you review and publish:

```sh
# 1. bump `version` in Cargo.toml, commit
git tag v0.2.0
git push origin v0.2.0
```

| Platform | Artifact | Built on |
|---|---|---|
| Linux | `MDV-<v>-x86_64.AppImage` | Ubuntu 22.04 (runs on glibc 2.35+ distros) |
| Windows | `MDV-<v>-windows-x86_64.exe` (portable, icon embedded) | windows-latest |
| macOS | `MDV-<v>-macos-universal.dmg` (Apple Silicon + Intel) | macos-latest |

Build the AppImage locally (downloads `appimagetool` into `dist/`):

```sh
./packaging/linux/build-appimage.sh
```

## Images

MDV renders both `![alt](path)` and HTML `<img>` tags, so READMEs with centered headers and side-by-side screenshots look right.

Resizing writes the size into the file. Plain markdown has no syntax for image size, so a resized image is saved as an HTML tag that GitHub, VS Code and most other renderers display at the same size:

```html
<img src="assets/screenshots/ss1.png" alt="MDV in dark mode" width="420">
```

Click an image to select it. Then drag a corner handle to resize it, press `Delete` to remove it, or double-click it to edit its markdown.

Web (`https://`) images are downloaded when the document opens, like in any markdown viewer.

## Text color

Markdown has no syntax for color, so MDV stores colored text as an HTML tag:

```html
<span style="color: #e5484d">important</span>
```

MDV, VS Code, Obsidian and Typora show the color. **GitHub doesn't**: it removes inline styles for safety, so on GitHub the text appears in the normal color (still readable, nothing breaks).

## How it works

**The markdown text is the only stored copy of the document.** Nothing is converted into a rich-text tree and back, so files are never rewritten behind your back, and switching between views loses nothing.

| File | Role |
|---|---|
| `src/doc.rs` | Text + selection (byte offsets) + undo/redo history + formatting commands (bold, headings, lists…) |
| `src/markdown.rs` | `pulldown-cmark` → top-level blocks with source offsets; style runs for syntax highlighting |
| `src/images.rs` | Parsing `<img>` tags, resolving image paths, writing image markdown |
| `src/find.rs` | Find & replace (search, highlights, the find bar) |
| `src/live.rs` | The WYSIWYG editor: rendering, caret/selection, keyboard and mouse editing |
| `src/ops.rs` | Source rules behind editing: escaping, keeping `**`/links balanced on delete and Enter |
| `src/editor.rs` | The Source view, plus helpers shared by both views |
| `src/theme.rs` | Dark/light palettes, text formats |
| `src/fonts.rs` | Uses the desktop's sans/mono fonts via fontconfig; Phosphor icons |
| `src/app.rs` | Window, toolbar, shortcuts, open/save dialogs (XDG portal), unsaved-changes prompt |
| `packaging/` | AppImage build, Linux user install, macOS `Info.plist` |
| `.github/workflows/` | CI (lint + tests) and release builds |

**Editor view** (`src/live.rs`): everything is drawn formatted, and every visible character knows the byte range of markdown source it came from. Caret moves, selection, typing and deleting happen on the visible text and are translated into source edits. `src/ops.rs` keeps the markup valid while doing that:
- Typed characters that mean something in markdown (`*`, `_`, `[` …) are escaped, so they stay literal.
- Deleting all of a bold word removes its `**` markers too, instead of leaving `****` behind.
- Pressing Enter inside bold or a link closes the formatting before the break and reopens it after.

**Source view:** the same `Doc`, shown as monospace highlighted text. The selection is shared, so the caret stays put when you switch.

## Shortcuts

| Keys | Action |
|---|---|
| `Ctrl+/` | Toggle editor ↔ source |
| `Ctrl+B` / `Ctrl+I` / `Ctrl+Shift+X` / `Ctrl+E` | Bold / italic / strikethrough / inline code (with nothing selected: applies to what you type next) |
| `Ctrl+K` | Insert link (asks for the URL) |
| `Ctrl+\` | Clear formatting |
| `Ctrl+0…6` | Paragraph / heading level 1–6 |
| `Ctrl+Z`, `Ctrl+Shift+Z` / `Ctrl+Y` | Undo, redo |
| `Ctrl+N` / `Ctrl+O` / `Ctrl+S` / `Ctrl+Shift+S` | New / open / save / save as |
| `Ctrl+F` / `Ctrl+H` | Find / find and replace |
| `Enter` / `Shift+Enter` (in find) or `F3` / `Shift+F3` | Next / previous match |
| `Ctrl+click` | Open a link |
| `Delete` (image selected) | Remove the image |
| `Shift+Enter` | Line break within a paragraph |
| `Esc` | Stop editing / deselect image / close find |

## Tests

```sh
cargo test
```

Includes headless UI tests (`egui_kittest`) that click and type into the live editor.

## Known limitations / next steps

- Markdown has no empty paragraphs, so pressing Enter twice doesn't leave a blank line.
- Empty table cells can't be typed into from the editor yet (use the Source view).
- Images inside a line of text (like badges) are shown after that line rather than inline.
- Pasting an image from the clipboard isn't supported yet; save it as a file and insert or drop it.
- Find matches the markdown source, so a search can't span formatting markers (`**bold**`).
- Theme follows the system at startup; the choice isn't persisted.

## License

[MIT](LICENSE)
