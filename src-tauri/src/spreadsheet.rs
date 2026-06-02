use std::path::Path;

use calamine::{Data, Reader, open_workbook_auto};
use serde::Serialize;

/// Name of the worksheet that holds the batch data. Matched case-insensitively.
pub const SHEET_NAME: &str = "Batcher";

/// One usable column of the Batcher sheet: row 1 names the image collection
/// (a top-level folder under `Images/`), row 2 names the language. A column is
/// only kept when it has both.
#[derive(Clone, Serialize)]
pub struct LangColumn {
    pub language: String,
    pub collection: String,
    /// 0-based index into a `MessageRow.cells` vector (and the source sheet column).
    pub column: usize,
}

/// One message row (rows 3+). `id` comes from column A. `cells` are the localized
/// texts aligned 1:1 with `SheetData.columns` (empty string = no text for that column).
#[derive(Clone, Serialize)]
pub struct MessageRow {
    pub id: String,
    pub cells: Vec<String>,
}

/// Parsed contents of the Batcher sheet.
#[derive(Clone, Serialize)]
pub struct SheetData {
    pub columns: Vec<LangColumn>,
    pub messages: Vec<MessageRow>,
}

/// True if `path` has an extension that `parse()` knows how to read.
/// Used by the working-folder scan so discovery and parsing agree.
pub fn is_supported(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("xlsx" | "xlsm" | "xlsb" | "xls" | "ods")
    )
}

/// Parse the `Batcher` sheet of `path` into the (columns × messages) model.
pub fn parse(path: &Path) -> Result<SheetData, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => parse_workbook(path),
        other => Err(format!(
            "unsupported spreadsheet format: .{other} (expected an .xlsx with a \"{SHEET_NAME}\" sheet)"
        )),
    }
}

fn parse_workbook(path: &Path) -> Result<SheetData, String> {
    let mut workbook = open_workbook_auto(path).map_err(|e| format!("open workbook: {e}"))?;

    let sheet_name = workbook
        .sheet_names()
        .into_iter()
        .find(|n| n.eq_ignore_ascii_case(SHEET_NAME))
        .ok_or_else(|| format!("no \"{SHEET_NAME}\" sheet found in the workbook"))?;

    let range = workbook
        .worksheet_range(&sheet_name)
        .map_err(|e| format!("read sheet {sheet_name}: {e}"))?;

    let mut iter = range.rows();
    let collection_row = iter.next().ok_or_else(|| "sheet is empty".to_string())?;
    let language_row = iter
        .next()
        .ok_or_else(|| "sheet has no language row (row 2)".to_string())?;

    let collections: Vec<String> = collection_row.iter().map(cell_to_string).collect();
    let languages: Vec<String> = language_row.iter().map(cell_to_string).collect();

    // A column is usable when it has both a collection (row 1) and a language
    // (row 2). Column A (index 0) holds the message id and is never a data column.
    let mut columns = Vec::new();
    let width = collections.len().max(languages.len());
    for col in 1..width {
        let collection = collections.get(col).cloned().unwrap_or_default();
        let language = languages.get(col).cloned().unwrap_or_default();
        if collection.is_empty() || language.is_empty() {
            continue;
        }
        columns.push(LangColumn {
            language,
            collection,
            column: col,
        });
    }

    if columns.is_empty() {
        return Err(format!(
            "no usable column in the \"{SHEET_NAME}\" sheet: each column needs a collection (row 1) and a language (row 2)"
        ));
    }

    let mut messages = Vec::new();
    for record in iter {
        let cells: Vec<String> = record.iter().map(cell_to_string).collect();
        let id = cells.first().cloned().unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let row_cells: Vec<String> = columns
            .iter()
            .map(|c| cells.get(c.column).cloned().unwrap_or_default())
            .collect();
        // Skip rows with no text at all across every usable column.
        if row_cells.iter().all(|c| c.is_empty()) {
            continue;
        }
        messages.push(MessageRow { id, cells: row_cells });
    }

    Ok(SheetData { columns, messages })
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

/// Sanitize a string for use as a single path component (folder or file stem),
/// cross-platform. Shared by the pipeline for language folders and message ids.
pub fn sanitize_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => out.push('_'),
            _ => out.push(c),
        }
    }
    out.trim().trim_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn example() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../Input/Batcher.xlsx")
    }

    #[test]
    fn parses_batcher_sheet() {
        let data = parse(&example()).expect("parse Batcher sheet");
        // English→Base and French→African are the two filled columns.
        assert!(
            data.columns
                .iter()
                .any(|c| c.language == "English" && c.collection.eq_ignore_ascii_case("Base")),
            "missing English/Base column, got {:?}",
            data.columns.iter().map(|c| (&c.language, &c.collection)).collect::<Vec<_>>()
        );
        assert!(data.columns.iter().any(|c| c.language == "French"));
        assert!(!data.messages.is_empty(), "no messages parsed");
        // Message ids should be normalized integers (1.0 → "1").
        assert_eq!(data.messages[0].id, "1");
        // Each message has one cell per column.
        assert_eq!(data.messages[0].cells.len(), data.columns.len());
    }
}
