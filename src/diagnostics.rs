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
                        let found = symbols.lookup(ref_file, ref_col, &cell.value).is_some()
                            || schema
                                .and_then(|s| s.enum_values_for_target(ref_file, ref_col))
                                .map(|vals| {
                                    vals.iter().any(|v| v.eq_ignore_ascii_case(&cell.value))
                                })
                                .unwrap_or(false);
                        if !found {
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
