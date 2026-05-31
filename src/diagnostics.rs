use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

use crate::document::DocumentData;
use crate::schema::{FieldTypeName, Schema};
use crate::workspace::SymbolIndex;

/// Validate a single document against the schema and symbol index.
///
/// Three classes of diagnostic are produced:
///   ERROR   — cross-reference target not found in the workspace symbol index
///   WARNING — value cannot be parsed as the column's declared int/float type
///   INFO    — column header is not declared in the schema and not in ignoreFields
pub fn validate_document(
    file_stem: &str,
    doc: &DocumentData,
    schema: Option<&Schema>,
    symbols: &SymbolIndex,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    let schema_file = schema.and_then(|s| s.get_file(file_stem));

    // Header-line unknown-column check (line 0).
    if let (Some(s), Some(sf)) = (schema, schema_file) {
        let mut col_offset: u32 = 0;
        for header in &doc.headers {
            let col_start = col_offset;
            let col_end = col_start + header.chars().count() as u32;
            col_offset = col_end + 1; // +1 for delimiter character

            if header.is_empty() {
                continue;
            }
            let known = s.find_field(file_stem, header).is_some()
                || sf.ignore_fields.iter().any(|ig| ig == header);
            if !known {
                diags.push(Diagnostic {
                    range: Range {
                        start: Position { line: 0, character: col_start },
                        end: Position { line: 0, character: col_end },
                    },
                    severity: Some(DiagnosticSeverity::INFORMATION),
                    source: Some("vector-lsp".into()),
                    message: format!("Column '{header}' is not defined in the schema"),
                    ..Default::default()
                });
            }
        }
    }

    // Row-level diagnostics.
    for row in &doc.rows {
        for (col_idx, cell) in row.cells.iter().enumerate() {
            if cell.value.is_empty() {
                continue;
            }
            let col_name = match doc.headers.get(col_idx) {
                Some(h) if !h.is_empty() => h.as_str(),
                _ => continue,
            };

            let field_type = schema
                .and_then(|s| s.find_field(file_stem, col_name))
                .and_then(|f| f.field_type.as_ref());

            let Some(ft) = field_type else { continue };

            let cell_end = cell.col_start + cell.value.chars().count() as u32;
            let cell_range = Range {
                start: Position { line: row.line, character: cell.col_start },
                end: Position { line: row.line, character: cell_end },
            };

            match ft.type_name {
                FieldTypeName::Reference => {
                    if let (Some(ref_file), Some(ref_col)) =
                        (ft.file.as_deref(), ft.field.as_deref())
                    {
                        if symbols.lookup(ref_file, ref_col, &cell.value).is_none() {
                            diags.push(Diagnostic {
                                range: cell_range,
                                severity: Some(DiagnosticSeverity::ERROR),
                                source: Some("vector-lsp".into()),
                                message: format!(
                                    "'{}' not found in {}.{}",
                                    cell.value, ref_file, ref_col
                                ),
                                ..Default::default()
                            });
                        }
                    }
                }
                FieldTypeName::Int => {
                    if cell.value.parse::<i64>().is_err() {
                        diags.push(Diagnostic {
                            range: cell_range,
                            severity: Some(DiagnosticSeverity::WARNING),
                            source: Some("vector-lsp".into()),
                            message: format!(
                                "'{}' is not a valid integer for column '{col_name}'",
                                cell.value
                            ),
                            ..Default::default()
                        });
                    }
                }
                FieldTypeName::Float => {
                    if cell.value.parse::<f64>().is_err() {
                        diags.push(Diagnostic {
                            range: cell_range,
                            severity: Some(DiagnosticSeverity::WARNING),
                            source: Some("vector-lsp".into()),
                            message: format!(
                                "'{}' is not a valid number for column '{col_name}'",
                                cell.value
                            ),
                            ..Default::default()
                        });
                    }
                }
                _ => {}
            }
        }
    }

    diags
}
