# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this app does

**Gravyr Batcher** is a Tauri 2 + Rust desktop app (macOS + Windows). The user picks a single **working folder** containing an XLSX with a **`Batcher` sheet** of localized texts and an `Images/` subfolder of source-image **collections**. The flow is **two screens**:

1. **Folder screen** — pick/drop the working folder. The backend scans it (`scan_workspace`).
2. **Layout screen** — browse the images of each collection, assign a **layout template** per image (`template-1` = the historical positions, or `custom` = manual logo/text placement), live-preview the result (approximate, in CSS/HTML), then generate **all** images or **only the selected** ones.

The app renders one branded image per `(language-column × image-of-its-collection × message-row × format)` — overlaying a vertical gradient, the localized text, and the Regus logo. Output is one folder per **language**.

### Spreadsheet model (the `Batcher` sheet)
- **Row 1** = image **collection** per column (`B='Base'`, `C='African'`, …) — a top-level folder under `Images/` (matched case-insensitively).
- **Row 2** = **language** per column (`B='English'`, `C='French'`, …).
- **Rows 3+** = `A = message id` (`1.0 → "1"`), then the localized text per column.
- A column is only used when it has **both** a collection (row 1) and a language (row 2). `English` pulls from `Images/Base`, `French` from `Images/african`, etc. — each language renders **only** its collection's images.

Expected working-folder layout:

```
Working folder/
├─ Images/
│  ├─ Base/      (collection — jpg/jpeg/png, nested subfolders walked recursively)
│  └─ african/   (collection)
└─ file.xlsx     (first .xlsx at the root; its "Batcher" sheet is read)
```

It is a **sibling of [Gravyr Cropper](../Gravyr%20Cropper/)** and deliberately mirrors its conventions: minisign-signed GitHub Releases updater flow, same unsigned distribution. Output goes to `<working_folder>/Output/<language>/`.

## Commands

```bash
# Dev — launches the Tauri window with hot reload + devtools open
npm run dev

# Production build (creates a .dmg on macOS, .exe + .msi on Windows)
npm run build

# Frontend-only build (sanity check Vite + JS without invoking cargo)
npm run vite:build

# Backend-only sanity check (much faster than `npm run build`)
cd src-tauri && cargo check

# Regenerate the app icon set after editing logo.png
npx tauri icon logo.png

# Regenerate the placeholder logo.png (procedural — no external deps)
node scripts/make-base-logo.mjs logo.png
```

A few backend tests run against the bundled `Input/` folder (`cd src-tauri && cargo test`). Manual verification: `npm run dev`, pointing the app at a working folder shaped like the layout above. The bundled `Input/` folder is ready to use: it has `Batcher.xlsx` (whose `Batcher` sheet holds the data) and `Images/{Base,african}/…`.

## Architecture

### Data flow

```
Frontend (src/main.js)              Rust backend (src-tauri/src/)
─────────────────────               ───────────────────────────────
pick/drop folder ──► invoke("scan_workspace")  ──► pipeline::scan_workspace() → Workspace
                                                    ├── spreadsheet::parse()  (Batcher sheet → columns + messages)
                                                    └── resolve each collection dir + walk its images
   ▼ (layout screen: per-image template, selection, CSS preview)
"Proceed …"      ──► invoke("process_batch", {plan}) ──► pipeline::process(plan)
                                                    ├── compositor::rasterize_logos()  (resvg, once)
                                                    ├── compositor::aeonik()           (FontRef, once)
                                                    └── rayon par_iter over (image, format)  [one Job each]
                                                        ├── resolve_layout(spec,format)  (template-1 | custom → px)
                                                        ├── prepare_canvas  (decode → crop → resize → gradient → logo@pos)
                                                        └── for each (language-column, message) on this image:
                                                            └── render_with_text  (clone → draw text → encode)
                                                                    ▲  emits "batcher://progress" per output
listen("batcher://progress")  ◄─────────────────────────────────────┘
```

The live preview on the layout screen is an **approximation rendered in CSS/HTML** (source image via Tauri's `convertFileSrc` asset protocol, plus a gradient/text/logo overlay positioned with the same constants). The final files are always rendered pixel-exact by the Rust compositor.

### Backend module split

- **[src-tauri/src/spreadsheet.rs](src-tauri/src/spreadsheet.rs)** — XLSX via `calamine`. `parse()` reads the **`Batcher` sheet** (matched case-insensitively, **not** the first sheet) into `SheetData { columns: Vec<LangColumn>, messages: Vec<MessageRow> }`. A `LangColumn` keeps its `language` (row 2) + `collection` (row 1) + sheet `column` index; a column is kept only when both are present. `sanitize_component` is the shared cross-platform path sanitizer.
- **[src-tauri/src/compositor.rs](src-tauri/src/compositor.rs)** — image rendering. Embeds `Aeonik-Regular.ttf` + `logo_16-9.svg` via `include_bytes!` (logo rasterized once with resvg → `RgbaImage`). `LayoutSpec` (`Template1` | `Custom`, from the UI plan) → `resolve_layout(spec, logo_w, logo_h) -> Layout { text_origin, text_max_width, logo_pos }` on the fixed 1080×1920 canvas. `prepare_canvas(source, logo, logo_pos)` builds the composed 9:16 canvas; `compose_text(...)` clones it and draws text → `RgbImage`; `encode_output(canvas, format, kind, path)` writes it (`Portrait916` as-is, `Square1x1` center-cropped).
- **[src-tauri/src/pipeline.rs](src-tauri/src/pipeline.rs)** — orchestrator. `scan_workspace` parses the sheet and resolves each referenced collection to `Images/<name>` (case-insensitive), listing its images (with `rel`/`subfolder` for UI grouping). `process(app, folder, plan)` builds one `Job` per `(language-column, image)`; each render composes the 9:16 canvas once and is encoded to every `FORMATS` entry (9:16 + 1:1). Pre-creates one dir per language, kicks rayon, emits progress. `Plan` carries `default_template`, `templates_by_image` (keyed by absolute path), `only_selected`, `selected`.
- **[src-tauri/src/lib.rs](src-tauri/src/lib.rs)** — Tauri builder, plugin registration, command handlers: `scan_workspace`, `preview_batch`, `process_batch`.

### Pre-stage caching (key perf decision)

There is **one composition per image**: the source is cropped to 9:16, resized to 1080×1920, gradient-applied, and logo-composited **exactly once**. Then for every `(language-column, message)` that targets it we clone the canvas and draw text (`compose_text`), and that one text-drawn canvas is encoded to **both** formats (`encode_output`). Memory peak per worker stays at one canvas (~8 MB).

### Format mapping (single composition, two encodes)

Everything composes on **one 9:16 canvas (1080×1920)**. The 1:1 output is a **centered crop** of that composed image — there is no independent square layout.

| `Format` variant | Output size | Derivation | Filename suffix |
|---|---|---|---|
| `Portrait916` | 1080×1920 | the composed canvas, encoded as-is | `_9x16` |
| `Square1x1`   | 1080×1080 | `crop(0, (1920-1080)/2, 1080, 1080)` of the same canvas | `_1x1` |

⚠️ Because 1:1 is the **vertical center** of the 9:16 (rows 420–1500), content placed near the top of the 9:16 (e.g. `template-1` text at y≈350) is **cropped out of the 1:1**. That's by design — the safe zone (below) marks the region that survives the crop; place text there via `custom` for a clean 1:1.

### Visual spec source

`template-1`'s compositing positions (gradient cutoff 40.183%, text origin `(104, 350)`, logo origin `(357, 1297)`, on the 1080×1920 canvas) come from the Regus file `BYPe40s5X77JiwS8HjQwRi`, frame `533:2535` (the 9x16 frame). The layout screen is designed in frames `603:4320` (template-1) and `603:4564` (custom). The **`custom`** template computes positions from canvas-relative anchors/fractions in the same 1080×1920 space. The frontend CSS preview in [src/main.js](src/main.js) duplicates these constants (`COMP`, `T1_TEXT_ORIGIN`, `T1_LOGO_POS`, `LOGO_SIZE`) — **keep the two in sync**. The preview shows the 9:16 composition in a clipping viewport; the 1:1 view simply shifts the composition up to reveal its center.

**Safe zones** (preview guide, per aspect): 9:16 = left/right 6 %, top 25 %, bottom 40 %; 1:1 = 6 % all around.

### Font fallback

Aeonik is embedded via `include_bytes!`. Before rendering, we scan the text's codepoints; if any character is `.notdef` in Aeonik, the whole string falls back to **system Arial** (loaded lazily from `/System/Library/Fonts/Supplemental/Arial.ttf` on macOS or `C:\Windows\Fonts\arial.ttf` on Windows). Per-string fallback, not per-glyph — keeps consistent type within a single text.

### Drag-and-drop

The whole window is **one drop target** for the working folder ([src/main.js](src/main.js)'s `onDragDropEvent` highlights `#dropzone-folder` and calls `setWorkingFolder(paths[0])` on drop). The picker (`pick-folder`) opens `open({ directory: true })`. `setWorkingFolder` invokes `scan_workspace`; on success it switches to the layout screen, otherwise the error surfaces in `#folder-hint`. The three screens (`#screen-folder`, `#screen-layout`, `#screen-progress`) are plain sections toggled by `showScreen(name)`.

## Output convention

One folder per **language**, keeping the source subfolder structure inside:

```
<working_folder>/Output/<language>/<subfolder…>/<image_stem>_<message_id>_<format_suffix>.<original_ext>
```

Each `(image, message)` yields one file per selected format, e.g. `Input/Output/French/9x16 v2/14_1_9x16.png` and `…_1_1x1.png` (from `Images/african/9x16 v2/14.png`). The `<language>` comes from row 2 of the Batcher sheet; the `Images/<collection>` level is dropped (the collection is implied by the language) but the source **subfolders** (e.g. `9x16 v2`, `Feb 26…/9x16`) are preserved. The output root is `<working_folder>/Output/` (see `pipeline::output_root`); per-image output dirs are pre-created from the unique set.

## Embedded assets

Two files in [Data/](Data/) are baked into the binary at compile time via `include_bytes!` (paths relative to `src-tauri/src/compositor.rs`):
- `../../Data/Aeonik-Regular.ttf`
- `../../Data/logo_16-9.svg` — the only logo (the 1:1 output is a crop of the 9:16, so it shares this logo). `Data/logo_1x1.svg` is no longer used by the build; the frontend preview imports `logo_16-9.svg` too.

If either is missing at build time, `cargo build` will fail. Replacing them rebuilds with the new asset — no other changes needed.

## Releases / updater

GitHub Actions workflow at [.github/workflows/release.yml](.github/workflows/release.yml) triggers on tags `v*`. Tag a version, push, and a **draft** release is created with universal-macOS DMG + Windows NSIS/MSI + a `latest.json` consumed by the in-app updater.

The workflow needs two GitHub repository secrets: `TAURI_SIGNING_PRIVATE_KEY` (multi-line content of `.minisign/batcher.key`) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The matching public key lives in [src-tauri/tauri.conf.json](src-tauri/tauri.conf.json) under `plugins.updater.pubkey`.

The `.minisign/` directory is **gitignored**. The private key never goes to the remote.

## Things to be careful about

- **NASM is required to build `turbojpeg`** (locally and in CI). On macOS: `brew install nasm`. The release workflow installs it on both runners.
- **Don't add a backwards-compat layer for canvas-relative positioning.** The text/logo/gradient positions in `compositor.rs` are tied to the 1080×1920 canvas — if you change canvas dimensions, recompute every constant from the Figma source rather than scaling at runtime.
- **1:1 is a centered crop of the composed 9:16, never an independent layout.** Don't add square-specific text/logo constants. If you change the crop, update `encode_output` and the preview's `--comp-top`/safe-zone insets together.
- **The output folder lives at the working-folder root** (`<working_folder>/Output/`), alongside `Images/` and the spreadsheet. Don't relocate it to inside `Images/` or to a user-chosen path without an explicit ask.
- **Images are discovered only under `<working_folder>/Images/<collection>/`** (recursively), where `<collection>` is a name referenced in row 1 of the Batcher sheet (matched case-insensitively). Collections referenced but missing on disk surface as `Workspace.warnings`. The spreadsheet is the first `.xlsx` at the root and **its `Batcher` sheet** is read — don't fall back to the first sheet.
- **The CSS preview is approximate, the Rust render is the source of truth.** Don't try to make the preview pixel-perfect; if the two disagree on positions, fix the shared constants, not the renderer.
- **The asset protocol is enabled** (`tauri.conf.json` → `app.security.assetProtocol`, with the `protocol-asset` Cargo feature) so the webview can show source images via `convertFileSrc`. Scope is currently `["**"]` because the user points the app at arbitrary folders.
- **Pre-rasterized logos are RgbaImage with straight (un-premultiplied) alpha.** `compositor::overlay_rgba()` assumes this — if you swap the rasterizer or change tiny-skia's pixel convention, update the un-premultiplication loop in `rasterize_svg()`.
