use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::compositor::{Format, Kind, prepare_canvas, render_with_text};
use crate::spreadsheet;

const PROGRESS_EVENT: &str = "batcher://progress";
const OUTPUT_DIR_NAME: &str = "Output";
const FORMATS: &[Format] = &[Format::Square, Format::Portrait916];

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

pub fn gather_images(inputs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in inputs {
        if p.is_file() {
            if Kind::from_path(p).is_some() {
                out.push(p.clone());
            }
        } else if p.is_dir() {
            // top-level only, like Cropper — skip the Output/ subfolder explicitly.
            let Ok(entries) = fs::read_dir(p) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && Kind::from_path(&path).is_some() {
                    out.push(path);
                }
            }
        }
    }
    out
}

fn output_root(image_inputs: &[PathBuf]) -> PathBuf {
    let first = image_inputs
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."));
    let base = if first.is_dir() {
        first
    } else {
        first.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
    };
    base.join(OUTPUT_DIR_NAME)
}

pub fn preview(spreadsheet_path: &Path, image_inputs: Vec<PathBuf>) -> Result<PreviewResult, String> {
    let rows = spreadsheet::parse(spreadsheet_path)?;
    let images = gather_images(&image_inputs);
    let texts: usize = rows.iter().map(|r| r.texts.len()).sum();
    let total = texts * images.len() * FORMATS.len();
    Ok(PreviewResult {
        rows: rows.len(),
        images: images.len(),
        total_outputs: total,
    })
}

pub fn process(
    app: AppHandle,
    spreadsheet_path: PathBuf,
    image_inputs: Vec<PathBuf>,
) -> Result<ProcessResult, String> {
    let rows = spreadsheet::parse(&spreadsheet_path)?;
    let images = gather_images(&image_inputs);
    let root = output_root(&image_inputs);
    let root_str = root.to_string_lossy().to_string();

    if rows.is_empty() {
        return Ok(ProcessResult {
            total: 0,
            ok: 0,
            errors: Vec::new(),
            skipped_no_image: false,
            skipped_no_rows: true,
            output_root: root_str,
        });
    }
    if images.is_empty() {
        return Ok(ProcessResult {
            total: 0,
            ok: 0,
            errors: Vec::new(),
            skipped_no_image: true,
            skipped_no_rows: false,
            output_root: root_str,
        });
    }

    // Pre-create row directories.
    for row in &rows {
        let dir = root.join(&row.folder_name);
        fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }

    let logos = crate::compositor::rasterize_logos()?;
    let aeonik = crate::compositor::aeonik()?;

    // Build jobs: one per (image, format). Each job carries every (row, text) target.
    struct Render {
        out_path: PathBuf,
        text: String,
    }
    struct Job {
        image: PathBuf,
        format: Format,
        renders: Vec<Render>,
    }

    let mut jobs: Vec<Job> = Vec::new();
    for image in &images {
        let stem = image
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("image")
            .to_string();
        let ext = image
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("jpg")
            .to_string();
        for &format in FORMATS {
            let mut renders = Vec::new();
            for row in &rows {
                let row_dir = root.join(&row.folder_name);
                for text in &row.texts {
                    let safe_col = sanitize_filename(&text.column_header);
                    let filename = format!("{stem}_{safe_col}_{}.{ext}", format.suffix());
                    renders.push(Render {
                        out_path: row_dir.join(filename),
                        text: text.text.clone(),
                    });
                }
            }
            jobs.push(Job {
                image: image.clone(),
                format,
                renders,
            });
        }
    }

    let total: usize = jobs.iter().map(|j| j.renders.len()).sum();
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

        match prepare_canvas(&job.image, job.format, &logos) {
            Ok(stage) => {
                for r in &job.renders {
                    let result = render_with_text(&stage, &r.text, &aeonik, &r.out_path);
                    let file_label = r
                        .out_path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    let (ok, err_msg) = match &result {
                        Ok(()) => {
                            ok_counter.fetch_add(1, Ordering::Relaxed);
                            (true, None)
                        }
                        Err(e) => {
                            errors.lock().unwrap().push(ProcessError {
                                file: file_label.clone(),
                                error: e.clone(),
                            });
                            (false, Some(e.clone()))
                        }
                    };
                    let current = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    let _ = app.emit(
                        PROGRESS_EVENT,
                        Progress {
                            current,
                            total,
                            file: file_label,
                            ok,
                            error: err_msg,
                        },
                    );
                }
            }
            Err(e) => {
                // The whole stage failed — surface one error per render and bump the counter.
                for r in &job.renders {
                    let file_label = r
                        .out_path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    errors.lock().unwrap().push(ProcessError {
                        file: format!("{image_label} → {file_label}"),
                        error: e.clone(),
                    });
                    let current = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    let _ = app.emit(
                        PROGRESS_EVENT,
                        Progress {
                            current,
                            total,
                            file: file_label,
                            ok: false,
                            error: Some(e.clone()),
                        },
                    );
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

fn sanitize_filename(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => out.push('_'),
            _ => out.push(c),
        }
    }
    out.trim().trim_matches('.').to_string()
}
