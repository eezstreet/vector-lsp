use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use serde::Deserialize;

use crate::runtime::ScriptRuntime;

// ---------------------------------------------------------------------------
// Schema types
// ---------------------------------------------------------------------------

/// The primitive type of a field's value, as declared in the schema.
/// Covers all distinct `type` strings observed in the d2rdoc schema files.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FieldTypeName {
    Int,
    Float,
    Boolean,
    Text,
    String,
    Array,
    Object,
    /// Calc expression field (parsed separately for sub-cell diagnostics).
    Parse,
    /// Value references a row in another file.
    Reference,
    /// Documentation-only; not a real column.
    Comment,
    #[serde(other)]
    Unknown,
}

/// Type descriptor attached to each field definition.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FieldType {
    #[serde(rename = "type")]
    pub type_name: FieldTypeName,
    #[serde(default)]
    pub data_length: i64,
    #[serde(default)]
    pub mem_size: i64,
    /// For `reference` fields: the file containing the target rows.
    pub file: Option<String>,
    /// For `reference` fields: the column in the target file to resolve against.
    pub field: Option<String>,
}

/// Declares that this field's valid values come from an enum defined in another schema file.
#[derive(Debug, Deserialize, Clone)]
pub struct AppendField {
    pub file: String,
    pub field: String,
}

/// A single column definition within a schema file.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SchemaField {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub field_type: Option<FieldType>,
    /// Alternative column names this field may appear under.
    #[serde(default)]
    pub alt_names: Vec<String>,
    /// If set, valid values for this field are drawn from the named enum table.
    pub append_field: Option<AppendField>,
    /// Enum table for `comment`-type fields in reference-only schema files.
    /// Each inner vec is [code, description].
    pub table: Option<Vec<Vec<serde_json::Value>>>,
}

/// Top-level schema entry for a single .txt file (or a reference-only pseudo-file).
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SchemaFile {
    /// Human-readable title (usually the .txt filename).
    pub title: Option<String>,
    /// Human-readable overview of what this file controls.
    pub overview: Option<String>,
    /// If true, this entry is a reference table only and has no corresponding .txt file.
    #[serde(default)]
    pub guide_only: bool,
    /// Schema files whose fields are merged into this one for reference purposes.
    #[serde(default)]
    pub reference_files: Vec<String>,
    /// Schema files whose fields are prepended to this file's field list.
    #[serde(default)]
    pub append_files: Vec<String>,
    /// Ordered column definitions for this file.
    #[serde(default)]
    pub fields: Vec<SchemaField>,
    /// Columns that exist in the .txt but are intentionally undocumented / ignored.
    #[serde(default)]
    pub ignore_fields: Vec<String>,
    /// The schema file that this one depends on for code-level implementation.
    pub code_dependency: Option<String>,
    #[serde(default)]
    pub not_searchable: bool,
    #[serde(default)]
    pub no_html: bool,
}

impl SchemaFile {
    /// Find a field by its primary name or any alt name.
    pub fn find_field(&self, col_name: &str) -> Option<&SchemaField> {
        self.fields.iter().find(|f| {
            f.name == col_name || f.alt_names.iter().any(|a| a == col_name)
        })
    }
}

/// The full parsed schema for a workspace — a map from file stem to its definition.
#[derive(Debug, Default)]
pub struct Schema {
    pub files: HashMap<String, SchemaFile>,
}

impl Schema {
    /// Look up the schema entry for a file, by its stem (e.g. `"armor"` for `armor.txt`).
    pub fn get_file(&self, stem: &str) -> Option<&SchemaFile> {
        self.files.get(stem).or_else(|| self.files.get(&stem.to_lowercase()))
    }

    /// Find a field definition for `col_name` in `file_stem`, following the
    /// `appendFiles` chain. Returns the first match found.
    /// A visited set prevents infinite loops if the schema has cycles.
    pub fn find_field<'a>(&'a self, file_stem: &str, col_name: &str) -> Option<&'a SchemaField> {
        self.find_field_inner(file_stem, col_name, &mut HashSet::new())
    }

    fn find_field_inner<'a>(
        &'a self,
        file_stem: &str,
        col_name: &str,
        visited: &mut HashSet<String>,
    ) -> Option<&'a SchemaField> {
        if !visited.insert(file_stem.to_lowercase()) {
            return None; // cycle guard
        }
        let sf = self.get_file(file_stem)?;
        if let Some(f) = sf.find_field(col_name) {
            return Some(f);
        }
        // Walk appendFiles so that fields shared across multiple files are found.
        let appended = sf.append_files.clone();
        for stem in &appended {
            if let Some(f) = self.find_field_inner(stem, col_name, visited) {
                return Some(f);
            }
        }
        None
    }

    /// Return the set of `(file_stem, column_name)` pairs that are pointed at by
    /// `reference`-typed fields anywhere in the schema. Drives SymbolIndex population.
    pub fn reference_targets(&self) -> HashSet<(String, String)> {
        let mut targets = HashSet::new();
        for schema_file in self.files.values() {
            for field in &schema_file.fields {
                if let Some(ft) = &field.field_type {
                    if ft.type_name == FieldTypeName::Reference {
                        if let (Some(file), Some(col)) = (&ft.file, &ft.field) {
                            targets.insert((file.to_lowercase(), col.clone()));
                        }
                    }
                }
            }
        }
        targets
    }
}

// ---------------------------------------------------------------------------
// Description formatting
// ---------------------------------------------------------------------------

/// Format a schema description string for display in LSP hover output.
///
/// Handles two transformations:
/// - `<br>` / `<br/>` / `<br />` HTML line-break tags → Markdown double newline
/// - `$!file#field!$` cross-reference syntax → Markdown emphasis/code formatting
///   - `$!enums#EARMORTYPE!$`  →  `` `EARMORTYPE` (in *enums*) ``
///   - `$!monstats!$`          →  `*monstats*`
///   - `$!#Id!$`               →  `` `Id` ``
pub fn format_description(text: &str) -> String {
    let text = text
        .replace("<br />", "\n\n")
        .replace("<br/>", "\n\n")
        .replace("<br>", "\n\n");

    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();

    while let Some(start) = rest.find("$!") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("!$") {
            Some(end) => {
                push_cross_ref(&after[..end], &mut out);
                rest = &after[end + 2..];
            }
            None => {
                // No closing marker — emit literally and stop scanning.
                out.push_str("$!");
                rest = after;
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

fn push_cross_ref(content: &str, out: &mut String) {
    match content.find('#') {
        Some(i) => {
            let file  = &content[..i];
            let field = &content[i + 1..];
            match (file.is_empty(), field.is_empty()) {
                (false, false) => out.push_str(&format!("`{}` (in *{}*)", field, file)),
                (false, true)  => out.push_str(&format!("*{}*", file)),
                (true,  false) => out.push_str(&format!("`{}`", field)),
                (true,  true)  => {}
            }
        }
        None if !content.is_empty() => out.push_str(&format!("*{}*", content)),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Loader
// ---------------------------------------------------------------------------

/// Load every `.js` file from `dir` into `runtime` and return the resulting schema.
///
/// Each JS file is expected to assign into a global `files` object:
///   `files["armor"] = { title: "armor.txt", fields: [...], ... }`
///
/// The same `runtime` instance can be kept alive afterwards for plugin execution.
pub fn load_schema(runtime: &mut ScriptRuntime, dir: &Path) -> Result<Schema> {
    // Seed the global registry that every schema file writes into.
    runtime.exec("__init__", "var files = {};")?;

    let mut paths: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |ext| ext == "js"))
        .collect();

    // Sort for deterministic load order (mirrors how a browser would load them).
    paths.sort();

    for path in paths {
        runtime.exec_file(&path)?;
    }

    let json = runtime.eval_json("files")?;
    let files: HashMap<String, SchemaFile> = serde_json::from_value(json)?;
    Ok(Schema { files })
}
