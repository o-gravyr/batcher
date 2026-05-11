use std::path::Path;

use calamine::{Data, Reader, open_workbook_auto};
use serde::Serialize;

#[derive(Clone, Serialize)]
pub struct TextEntry {
    pub column_header: String,
    pub text: String,
}

#[derive(Clone, Serialize)]
pub struct Row {
    pub folder_name: String,
    pub texts: Vec<TextEntry>,
}

pub fn parse(path: &Path) -> Result<Vec<Row>, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "csv" => parse_csv(path),
        "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => parse_workbook(path),
        other => Err(format!("unsupported spreadsheet format: .{other}")),
    }
}

fn parse_csv(path: &Path) -> Result<Vec<Row>, String> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_path(path)
        .map_err(|e| format!("open csv: {e}"))?;

    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| format!("read csv headers: {e}"))?
        .iter()
        .map(|s| s.to_string())
        .collect();

    if headers.len() < 2 {
        return Err("spreadsheet needs at least 2 columns: folder name + at least one text column".into());
    }

    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| format!("read csv row: {e}"))?;
        let cells: Vec<String> = record.iter().map(|s| s.to_string()).collect();
        if let Some(row) = build_row(&headers, &cells) {
            rows.push(row);
        }
    }

    Ok(rows)
}

fn parse_workbook(path: &Path) -> Result<Vec<Row>, String> {
    let mut workbook = open_workbook_auto(path).map_err(|e| format!("open workbook: {e}"))?;
    let sheet_name = workbook
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| "workbook has no sheets".to_string())?;
    let range = workbook
        .worksheet_range(&sheet_name)
        .map_err(|e| format!("read sheet {sheet_name}: {e}"))?;

    let mut iter = range.rows();
    let header_row = iter
        .next()
        .ok_or_else(|| "sheet is empty".to_string())?;
    let headers: Vec<String> = header_row.iter().map(cell_to_string).collect();

    if headers.len() < 2 {
        return Err("spreadsheet needs at least 2 columns: folder name + at least one text column".into());
    }

    let mut rows = Vec::new();
    for record in iter {
        let cells: Vec<String> = record.iter().map(cell_to_string).collect();
        if let Some(row) = build_row(&headers, &cells) {
            rows.push(row);
        }
    }

    Ok(rows)
}

fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::String(s) => s.trim().to_string(),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.is_finite() && f.abs() < 1e15 {
                format!("{:.0}", f)
            } else {
                format!("{}", f)
            }
        }
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => b.to_string(),
        Data::DateTime(d) => d.to_string(),
        Data::DateTimeIso(s) => s.clone(),
        Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("#ERROR:{e:?}"),
        Data::Empty => String::new(),
    }
}

fn build_row(headers: &[String], cells: &[String]) -> Option<Row> {
    let folder_raw = cells.first().map(|s| s.as_str()).unwrap_or("").trim();
    let folder_name = sanitize_folder(folder_raw);
    if folder_name.is_empty() {
        return None;
    }

    let mut texts = Vec::new();
    for (idx, header) in headers.iter().enumerate().skip(1) {
        let cell = cells.get(idx).map(|s| s.as_str()).unwrap_or("").trim();
        if cell.is_empty() {
            continue;
        }
        texts.push(TextEntry {
            column_header: header.trim().to_string(),
            text: cell.to_string(),
        });
    }

    if texts.is_empty() {
        return None;
    }
    Some(Row { folder_name, texts })
}

fn sanitize_folder(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => out.push('_'),
            _ => out.push(c),
        }
    }
    out.trim().trim_matches('.').to_string()
}
