use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::compositor::{
    Format, Kind, LayoutSpec, compose_text, encode_output, prepare_canvas, rasterize_logo,
    resolve_layout,
};
use crate::spreadsheet::{self, LangColumn, MessageRow};

const PROGRESS_EVENT: &str = "batcher://progress";
const OUTPUT_DIR_NAME: &str = "Output";
const IMAGES_DIR_NAME: &str = "Images";
// Both outputs derive from one composed 9:16 canvas (1:1 is a centered crop).
const FORMATS: &[Format] = &[Format::Portrait916, Format::Square1x1];

#[derive(Serialize, Clone)]
struct Progress {
    current: usize,
    total: usize,
    file: String,
    ok: bool,
    error: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct ProcessError {
    pub file: String,
    pub error: String,
}

#[derive(Serialize)]
pub struct ProcessResult {
    pub total: usize,
    pub ok: usize,
    pub errors: Vec<ProcessError>,
    pub skipped_no_image: bool,
    pub skipped_no_rows: bool,
    pub output_root: String,
}

#[derive(Serialize)]
pub struct PreviewResult {
    pub rows: usize,
    pub images: usize,
    pub total_outputs: usize,
}

/// One source image inside a collection, with paths the UI needs to display and group it.
#[derive(Serialize, Clone)]
pub struct ImageInfo {
    /// Absolute path (used as the per-image template key and for `convertFileSrc`).
    pub path: String,
    /// Path relative to the collection folder (e.g. `9x16 v2/14.png`).
    pub rel: String,
    /// Relative parent directory under the collection, for UI grouping (`""` at root).
    pub subfolder: String,
    pub stem: String,
    pub ext: String,
}

#[derive(Serialize, Clone)]
pub struct Collection {
    /// Name as written in the sheet's row 1 (e.g. `Base`, `African`).
    pub name: String,
    /// Absolute resolved folder under `Images/`.
    pub folder: String,
    pub images: Vec<ImageInfo>,
}

/// Everything the layout screen needs: the parsed sheet plus the resolved image
/// collections, derived from one working folder.
#[derive(Serialize)]
pub struct Workspace {
    pub columns: Vec<LangColumn>,
    pub messages: Vec<MessageRow>,
    pub collections: Vec<Collection>,
    pub output_root: String,
    pub warnings: Vec<String>,
}

/// Generation plan sent by the UI alongside the working folder.
#[derive(Deserialize)]
pub struct Plan {
    /// Template applied to images without an explicit override.
    pub default_template: LayoutSpec,
    /// Per-image template overrides, keyed by absolute image path.
    #[serde(default)]
    pub templates_by_image: HashMap<String, LayoutSpec>,
    /// When true, only `selected` images are rendered.
    #[serde(default)]
    pub only_selected: bool,
    /// Absolute paths of the checked images (used when `only_selected`).
    #[serde(default)]
    pub selected: Vec<String>,
    /// Output format suffixes to export (e.g. ["9x16","1x1"]). Empty = all formats.
    #[serde(default)]
    pub formats: Vec<String>,
}

/// First spreadsheet file at the working-folder root, sorted for determinism.
/// Skips Office lock files (`~$…`).
fn find_spreadsheet(folder: &Path) -> Option<PathBuf> {
    let mut sheets: Vec<PathBuf> = fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && spreadsheet::is_supported(p))
        .filter(|p| {
            !p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("~$"))
                .unwrap_or(false)
        })
        .collect();
    sheets.sort();
    sheets.into_iter().next()
}

/// Resolve a collection name from the sheet to a top-level folder under `Images/`,
/// matching the directory name case-insensitively.
fn resolve_collection_dir(images_root: &Path, name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(images_root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case(name))
            .unwrap_or(false)
        {
            return Some(path);
        }
    }
    None
}

/// All jpg/jpeg/png under `dir`, recursively, sorted for determinism.
fn walk_images(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_images(&path, out);
        } else if path.is_file() && Kind::from_path(&path).is_some() {
            out.push(path);
        }
    }
}

fn output_root(folder: &Path) -> PathBuf {
    folder.join(OUTPUT_DIR_NAME)
}

/// Build an `ImageInfo` for `path` relative to its collection `root`.
fn image_info(path: &Path, root: &Path) -> ImageInfo {
    let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
    let subfolder = rel
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    ImageInfo {
        path: path.to_string_lossy().to_string(),
        rel: rel.to_string_lossy().replace('\\', "/"),
        subfolder,
        stem: path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("image")
            .to_string(),
        ext: path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("jpg")
            .to_string(),
    }
}

/// Parse the sheet and resolve every referenced collection's images.
pub fn scan_workspace(folder: &Path) -> Result<Workspace, String> {
    let sheet = find_spreadsheet(folder)
        .ok_or_else(|| "No spreadsheet (.xlsx) found in the working folder.".to_string())?;
    let data = spreadsheet::parse(&sheet)?;

    let images_root = folder.join(IMAGES_DIR_NAME);
    let mut warnings = Vec::new();

    // Distinct collection names, in first-seen order.
    let mut seen = HashSet::new();
    let mut names: Vec<String> = Vec::new();
    for col in &data.columns {
        if seen.insert(col.collection.clone()) {
            names.push(col.collection.clone());
        }
    }

    let mut collections = Vec::new();
    for name in names {
        match resolve_collection_dir(&images_root, &name) {
            Some(dir) => {
                let mut paths = Vec::new();
                walk_images(&dir, &mut paths);
                paths.sort();
                let images = paths.iter().map(|p| image_info(p, &dir)).collect::<Vec<_>>();
                if images.is_empty() {
                    warnings.push(format!("Collection \"{name}\" has no images under Images/."));
                }
                collections.push(Collection {
                    name,
                    folder: dir.to_string_lossy().to_string(),
                    images,
                });
            }
            None => {
                warnings.push(format!("Collection \"{name}\" not found under Images/."));
                collections.push(Collection {
                    name,
                    folder: String::new(),
                    images: Vec::new(),
                });
            }
        }
    }

    Ok(Workspace {
        columns: data.columns,
        messages: data.messages,
        collections,
        output_root: output_root(folder).to_string_lossy().to_string(),
        warnings,
    })
}

/// Count of non-empty cells for column index `i` across all messages.
fn texts_for_column(messages: &[MessageRow], i: usize) -> usize {
    messages
        .iter()
        .filter(|m| m.cells.get(i).map(|c| !c.is_empty()).unwrap_or(false))
        .count()
}

pub fn preview(folder: &Path) -> Result<PreviewResult, String> {
    let ws = scan_workspace(folder)?;
    let by_name: HashMap<&str, &Collection> =
        ws.collections.iter().map(|c| (c.name.as_str(), c)).collect();

    let mut total = 0usize;
    for (i, col) in ws.columns.iter().enumerate() {
        let images = by_name
            .get(col.collection.as_str())
            .map(|c| c.images.len())
            .unwrap_or(0);
        total += images * texts_for_column(&ws.messages, i) * FORMATS.len();
    }

    let images_total: usize = ws.collections.iter().map(|c| c.images.len()).sum();
    Ok(PreviewResult {
        rows: ws.messages.len(),
        images: images_total,
        total_outputs: total,
    })
}

struct Render {
    text: String,
    /// One output per format (9:16 + 1:1), both from the same composed canvas.
    outputs: Vec<(Format, PathBuf)>,
}

struct Job {
    image: PathBuf,
    spec: LayoutSpec,
    renders: Vec<Render>,
}

pub fn process(app: AppHandle, folder: PathBuf, plan: Plan) -> Result<ProcessResult, String> {
    let ws = scan_workspace(&folder)?;
    let root = output_root(&folder);
    let root_str = root.to_string_lossy().to_string();

    if ws.messages.is_empty() {
        return Ok(skipped(root_str, false, true));
    }
    let any_image = ws.collections.iter().any(|c| !c.images.is_empty());
    if !any_image {
        return Ok(skipped(root_str, true, false));
    }

    let selected: HashSet<&str> = plan.selected.iter().map(|s| s.as_str()).collect();
    let by_name: HashMap<&str, &Collection> =
        ws.collections.iter().map(|c| (c.name.as_str(), c)).collect();

    // Outputs go to Output/<language>/<source-subfolder>/ (one folder per language,
    // keeping the source subfolder structure inside).
    let mut out_dirs: HashSet<PathBuf> = HashSet::new();

    let logo = rasterize_logo()?;
    let aeonik = crate::compositor::aeonik()?;

    // Formats to export (from the UI checkboxes). Empty = all (robust fallback).
    let active_formats: Vec<Format> = if plan.formats.is_empty() {
        FORMATS.to_vec()
    } else {
        FORMATS
            .iter()
            .copied()
            .filter(|f| plan.formats.iter().any(|s| s == f.suffix()))
            .collect()
    };

    // Build one job per (column, image). Each render composes the 9:16 canvas once
    // and is encoded to every format (full 9:16 + centered 1:1 crop).
    let mut jobs: Vec<Job> = Vec::new();
    for (i, col) in ws.columns.iter().enumerate() {
        let Some(collection) = by_name.get(col.collection.as_str()) else {
            continue;
        };
        let lang_dir = root.join(spreadsheet::sanitize_component(&col.language));
        for image in &collection.images {
            if plan.only_selected && !selected.contains(image.path.as_str()) {
                continue;
            }
            let spec = plan
                .templates_by_image
                .get(&image.path)
                .cloned()
                .unwrap_or_else(|| plan.default_template.clone());

            // Output/<language>/<source-subfolder>/ — keep the source subfolders.
            let out_dir = if image.subfolder.is_empty() {
                lang_dir.clone()
            } else {
                lang_dir.join(&image.subfolder)
            };
            out_dirs.insert(out_dir.clone());

            let mut renders = Vec::new();
            for msg in &ws.messages {
                let Some(text) = msg.cells.get(i) else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                let msg_id = spreadsheet::sanitize_component(&msg.id);
                let outputs = active_formats
                    .iter()
                    .map(|&format| {
                        let filename =
                            format!("{}_{}_{}.{}", image.stem, msg_id, format.suffix(), image.ext);
                        (format, out_dir.join(filename))
                    })
                    .collect();
                renders.push(Render {
                    text: text.clone(),
                    outputs,
                });
            }
            if renders.is_empty() {
                continue;
            }
            jobs.push(Job {
                image: PathBuf::from(&image.path),
                spec: spec.clone(),
                renders,
            });
        }
    }

    // Pre-create every output directory once (idempotent; avoids races in the rayon loop).
    for dir in &out_dirs {
        fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }

    let total: usize = jobs
        .iter()
        .map(|j| j.renders.iter().map(|r| r.outputs.len()).sum::<usize>())
        .sum();
    let counter = AtomicUsize::new(0);
    let ok_counter = AtomicUsize::new(0);
    let errors: Mutex<Vec<ProcessError>> = Mutex::new(Vec::new());

    jobs.par_iter().for_each(|job| {
        let image_label = job
            .image
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        let layout = resolve_layout(&job.spec, logo.width(), logo.height());

        let emit = |file_label: String, ok: bool, err: Option<String>| {
            let current = counter.fetch_add(1, Ordering::Relaxed) + 1;
            let _ = app.emit(
                PROGRESS_EVENT,
                Progress {
                    current,
                    total,
                    file: file_label,
                    ok,
                    error: err,
                },
            );
        };

        match prepare_canvas(&job.image, &logo, layout.logo_pos) {
            Ok(stage) => {
                for r in &job.renders {
                    let composed = compose_text(
                        &stage,
                        &r.text,
                        &aeonik,
                        layout.text_origin,
                        layout.text_max_width,
                        layout.font_px,
                        layout.line_height,
                        layout.letter_spacing_px,
                    );
                    for (format, out_path) in &r.outputs {
                        let file_label = out_path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        match encode_output(&composed, *format, stage.kind, out_path) {
                            Ok(()) => {
                                ok_counter.fetch_add(1, Ordering::Relaxed);
                                emit(file_label, true, None);
                            }
                            Err(e) => {
                                errors.lock().unwrap().push(ProcessError {
                                    file: file_label.clone(),
                                    error: e.clone(),
                                });
                                emit(file_label, false, Some(e));
                            }
                        }
                    }
                }
            }
            Err(e) => {
                // The whole stage failed — surface one error per output and bump the counter.
                for r in &job.renders {
                    for (_format, out_path) in &r.outputs {
                        let file_label = out_path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        errors.lock().unwrap().push(ProcessError {
                            file: format!("{image_label} → {file_label}"),
                            error: e.clone(),
                        });
                        emit(file_label, false, Some(e.clone()));
                    }
                }
            }
        }
    });

    Ok(ProcessResult {
        total,
        ok: ok_counter.load(Ordering::Relaxed),
        errors: errors.into_inner().unwrap(),
        skipped_no_image: false,
        skipped_no_rows: false,
        output_root: root_str,
    })
}

fn skipped(output_root: String, no_image: bool, no_rows: bool) -> ProcessResult {
    ProcessResult {
        total: 0,
        ok: 0,
        errors: Vec::new(),
        skipped_no_image: no_image,
        skipped_no_rows: no_rows,
        output_root,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::{self, LayoutSpec};

    fn input_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../Input")
    }

    #[test]
    fn scans_collections_and_images() {
        let ws = scan_workspace(&input_dir()).expect("scan");
        let base = ws
            .collections
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case("Base"))
            .expect("Base collection");
        assert!(!base.images.is_empty(), "Base has no images");
        // Subfolder grouping is populated for nested images.
        assert!(base.images.iter().any(|i| !i.subfolder.is_empty()));
        // Preview counter is non-zero and consistent.
        let pv = preview(&input_dir()).expect("preview");
        assert!(pv.total_outputs > 0);
    }

    #[test]
    fn renders_one_real_image_both_templates() {
        let ws = scan_workspace(&input_dir()).expect("scan");
        let img = ws
            .collections
            .iter()
            .flat_map(|c| &c.images)
            .next()
            .expect("at least one image");
        let logo = compositor::rasterize_logo().unwrap();
        let aeonik = compositor::aeonik().unwrap();
        let tmp = std::env::temp_dir();

        for (i, spec) in [
            LayoutSpec::Template1,
            LayoutSpec::Custom {
                logo_x: compositor::Anchor::Right,
                logo_y: 0.5,
                text_x: compositor::Anchor::Center,
                text_y: 0.3,
                text_w: 0.6,
                font_px: 96.0,
                line_height: 1.3,
                letter_spacing: 5.0,
            },
        ]
        .into_iter()
        .enumerate()
        {
            let layout = resolve_layout(&spec, logo.width(), logo.height());
            // Compose the 9:16 canvas once, then encode both formats from it.
            let stage = prepare_canvas(Path::new(&img.path), &logo, layout.logo_pos).unwrap();
            let composed = compose_text(
                &stage,
                "Réinventez votre espace de travail",
                &aeonik,
                layout.text_origin,
                layout.text_max_width,
                layout.font_px,
                layout.line_height,
                layout.letter_spacing_px,
            );
            for &format in FORMATS {
                let out =
                    tmp.join(format!("batcher_test_{i}_{}.{}", format.suffix(), img.ext));
                encode_output(&composed, format, stage.kind, &out).unwrap();
                let meta = std::fs::metadata(&out).unwrap();
                assert!(meta.len() > 0, "empty output for {}", out.display());
                // The 1:1 must be square (centered crop), the 9:16 the full canvas.
                let dims = image::image_dimensions(&out).unwrap();
                match format {
                    Format::Square1x1 => assert_eq!(dims, (1080, 1080)),
                    Format::Portrait916 => assert_eq!(dims, (1080, 1920)),
                }
                let _ = std::fs::remove_file(&out);
            }
        }
    }

    // Some inputs carry the wrong extension (African images are .png but hold JPEG
    // bytes). Decoding must sniff content, not trust the extension — otherwise the
    // whole collection fails at export.
    #[test]
    fn renders_image_with_mismatched_extension() {
        let ws = scan_workspace(&input_dir()).expect("scan");
        let png = ws
            .collections
            .iter()
            .flat_map(|c| &c.images)
            .find(|i| i.ext.eq_ignore_ascii_case("png"))
            .expect("a .png source");
        let logo = compositor::rasterize_logo().unwrap();
        let aeonik = compositor::aeonik().unwrap();
        let layout = resolve_layout(&LayoutSpec::Template1, logo.width(), logo.height());
        let stage = prepare_canvas(Path::new(&png.path), &logo, layout.logo_pos)
            .expect("decode/prepare a .png that actually holds JPEG bytes");
        let composed = compose_text(
            &stage,
            "Test",
            &aeonik,
            layout.text_origin,
            layout.text_max_width,
            layout.font_px,
            layout.line_height,
            layout.letter_spacing_px,
        );
        let out = std::env::temp_dir().join(format!("batcher_ext_{}.{}", png.stem, png.ext));
        encode_output(&composed, Format::Portrait916, stage.kind, &out).unwrap();
        assert!(std::fs::metadata(&out).unwrap().len() > 0);
        let _ = std::fs::remove_file(&out);
    }
}
