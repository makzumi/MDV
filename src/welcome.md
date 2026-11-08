# Welcome to MDV

A **light, fast, native** markdown editor written in Rust. No browser engine, no webview — just your text, drawn on the GPU.

Click anywhere to start editing. The block under your caret reveals its *markdown syntax*; everything else stays rendered.

## Features

- Inline editing with live formatting
- Toggle the **Source** view with `Ctrl+/`
- Undo / redo, smart lists, and ~~clutter~~ keyboard shortcuts
- Open with `Ctrl+O`, save with `Ctrl+S`, or drop a file on the window
- Insert images from the toolbar or by dropping them in, then click one to resize it
- Find and replace with `Ctrl+F` / `Ctrl+H`

> Markdown is stored exactly as you wrote it. Switching views never rewrites your file.

### Tasks

- [x] Render markdown natively
- [x] Edit in place
- [ ] Your next great document

### Code

```rust
fn main() {
    println!("Hello from MDV!");
}
```

| Shortcut     | Action         |
|--------------|----------------|
| `Ctrl+B`     | Bold           |
| `Ctrl+I`     | Italic         |
| `Ctrl+K`     | Insert link    |
| `Ctrl+1…3`   | Heading level  |

---

Learn more about markdown at [commonmark.org](https://commonmark.org).
