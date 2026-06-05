mod backend;
mod cli;
mod diagnostics;
mod document;
mod plugin;
mod runtime;
mod schema;
mod settings;
mod workspace;

use std::collections::HashSet;
use std::sync::Arc;

use clap::Parser;
use config::{Config, Environment, File};
use tokio::sync::RwLock;
use tower_lsp::lsp_types::DiagnosticSeverity;
use tower_lsp::{LspService, Server};

use cli::CliArgs;
use document::DocumentData;
use runtime::ScriptRuntime;
use schema::load_schema;
use settings::{IoType, VectorLspSettings};
use workspace::{SymbolIndex, Workspace};

/// Run a one-shot workspace check: scan all data files, validate them, print diagnostics, and
/// return an exit code (0 = clean, 1 = errors found, 2 = configuration/IO error).
async fn run_check(settings: &VectorLspSettings) -> i32 {
    let workspace_path = match &settings.workspace_path {
        Some(p) => p.clone(),
        None => {
            eprintln!("error: single_shot mode requires `workspace_path` in config");
            return 2;
        }
    };

    // Load schema if configured.
    let schema_result = if let Some(schema_path) = &settings.schema_path {
        let schema_path = schema_path.clone();
        let plugin_path = settings.plugin_path.clone();
        match tokio::task::spawn_blocking(move || {
            ScriptRuntime::new()
                .and_then(|mut rt| load_schema(&mut rt, &schema_path, plugin_path.as_deref()))
        })
        .await
        {
            Ok(Ok(s)) => {
                eprintln!("Schema loaded.");
                Some(Arc::new(s))
            }
            Ok(Err(e)) => {
                eprintln!("error: schema load failed: {e:#}");
                return 2;
            }
            Err(e) => {
                eprintln!("error: schema task panicked: {e}");
                return 2;
            }
        }
    } else {
        None
    };

    let ref_targets: HashSet<(String, String)> = schema_result
        .as_ref()
        .map(|s| s.reference_targets())
        .unwrap_or_default();

    // Scan and parse workspace files.
    let ext = settings.extension.as_str();
    let delimiter = settings.delimiter_char();
    let mut entries = match std::fs::read_dir(&workspace_path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: cannot read workspace directory '{}': {e}", workspace_path.display());
            return 2;
        }
    };

    let mut parsed: Vec<(std::path::PathBuf, String, Arc<DocumentData>)> = Vec::new();
    loop {
        let entry = match entries.next() {
            Some(Ok(e)) => e,
            Some(Err(e)) => { eprintln!("warning: directory entry error: {e}"); continue; }
            None => break,
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some(ext) {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        match std::fs::read(&path).and_then(|b| Ok(settings.encoding.decode(&b))) {
            Ok(Ok(src)) => parsed.push((path, stem, Arc::new(DocumentData::parse(&src, delimiter)))),
            Ok(Err(e)) => eprintln!("warning: skipping '{}': {e}", path.display()),
            Err(e) => eprintln!("warning: skipping '{}': {e}", path.display()),
        }
    }

    // Build symbol index.
    let mut symbols = SymbolIndex::new();
    for (path, stem, doc) in &parsed {
        let Ok(uri) = tower_lsp::lsp_types::Url::from_file_path(path) else { continue };
        symbols.index_document(&uri, stem, doc, &ref_targets);
    }

    // Validate and collect diagnostics.
    let mut total_errors = 0usize;
    let mut total_warnings = 0usize;
    let mut file_count = 0usize;

    for (path, stem, doc) in &parsed {
        let diags = diagnostics::validate_document(stem, doc, schema_result.as_deref(), &symbols);
        if diags.is_empty() {
            continue;
        }
        file_count += 1;
        let display = path.display();
        for d in &diags {
            let line = d.range.start.line + 1;
            let col = d.range.start.character + 1;
            let severity = match d.severity {
                Some(DiagnosticSeverity::ERROR) => { total_errors += 1; "error" }
                Some(DiagnosticSeverity::WARNING) => { total_warnings += 1; "warning" }
                Some(DiagnosticSeverity::INFORMATION) => "info",
                _ => "hint",
            };
            println!("{display}:{line}:{col}: {severity}: {}", d.message);
        }
    }

    if total_errors == 0 && total_warnings == 0 {
        eprintln!("No diagnostics found across {} file(s).", parsed.len());
        0
    } else {
        eprintln!("{} error(s), {} warning(s) across {file_count} file(s).", total_errors, total_warnings);
        if total_errors > 0 { 1 } else { 0 }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = CliArgs::parse();

    let raw = Config::builder()
        .add_source(File::with_name(&args.config_file).required(false))
        .add_source(Environment::with_prefix("VLSP"))
        .build()?;

    let mut settings = raw.try_deserialize::<VectorLspSettings>().unwrap_or_default();
    if let Some(schema_path) = args.schema_path {
        settings.schema_path = Some(schema_path);
    }
    let settings = Arc::new(settings);

    if settings.single_shot || args.single_shot {
        let code = run_check(&settings).await;
        std::process::exit(code);
    }

    let workspace = Arc::new(RwLock::new(Workspace::new()));
    // PluginHost is Clone (wraps an mpsc::Sender) so it can be shared cheaply
    // across TCP connections without spawning additional threads.
    let plugin_host = if let Some(ref dir) = settings.plugin_path {
        let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("ts") | Some("js")
                ) && p.file_name().map_or(true, |n| n != "_patches.js")
            })
            .collect();
        paths.sort();
        if paths.is_empty() { None } else { Some(plugin::PluginHost::new(paths)) }
    } else {
        None
    };

    match settings.io_type.clone() {
        IoType::Stdio => {
            let stdin = tokio::io::stdin();
            let stdout = tokio::io::stdout();
            let (service, socket) = LspService::new(move |client| backend::Backend {
                client,
                settings: Arc::clone(&settings),
                workspace: Arc::clone(&workspace),
                plugin_host: plugin_host.clone(),
            });
            Server::new(stdin, stdout, socket).serve(service).await;
        }
        IoType::Tcp(tcp) => {
            let addr = format!("{}:{}", tcp.host, tcp.port);
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            loop {
                let (stream, _) = listener.accept().await?;
                let (read, write) = tokio::io::split(stream);
                let settings = Arc::clone(&settings);
                let workspace = Arc::clone(&workspace);
                let plugin_host = plugin_host.clone();
                let (service, socket) = LspService::new(move |client| backend::Backend {
                    client,
                    settings: Arc::clone(&settings),
                    workspace: Arc::clone(&workspace),
                    plugin_host: plugin_host.clone(),
                });
                tokio::spawn(async move {
                    Server::new(read, write, socket).serve(service).await;
                });
            }
        }
    }

    Ok(())
}
