use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tower_lsp::lsp_types::*;
use tower_lsp::{jsonrpc::Result as LspResult, Client, LanguageServer};

use crate::diagnostics;
use crate::document::DocumentData;
use crate::plugin;
use crate::runtime;
use crate::schema::{find_loader, format_description, FieldTypeName};
use crate::settings::VectorLspSettings;
use crate::workspace::Workspace;

pub struct Backend {
    pub client: Client,
    // Arc so the same settings can be shared across TCP connections cheaply.
    pub settings: Arc<VectorLspSettings>,
    pub workspace: Arc<RwLock<Workspace>>,
    /// None when no plugins are configured.
    pub plugin_host: Option<plugin::PluginHost>,
}

impl Backend {
    /// Extract the lowercase file stem from a URI (e.g. `"armor"` from `.../armor.txt`).
    fn file_stem(uri: &Url) -> String {
        let name = uri.path_segments().and_then(|s| s.last()).unwrap_or("");
        match name.rfind('.') {
            Some(i) => name[..i].to_lowercase(),
            None => name.to_lowercase(),
        }
    }

    /// Read a file from disk using the configured encoding.
    async fn read_file(&self, path: &std::path::Path) -> anyhow::Result<String> {
        let bytes = tokio::fs::read(path).await?;
        self.settings.encoding.decode(&bytes)
    }

    /// Scan all data files in the workspace root, parse and index them.
    /// Called after the schema (and thus ref_targets) is ready.
    async fn scan_and_index_workspace(&self) {
        let (root_uri, delimiter, ext) = {
            let ws = self.workspace.read().await;
            (
                ws.root_uri.clone(),
                self.settings.delimiter_char(),
                self.settings.extension.clone(),
            )
        };

        let Some(root_uri) = root_uri else { return; };
        let Ok(root_path) = root_uri.to_file_path() else { return; };

        let mut read_dir = match tokio::fs::read_dir(&root_path).await {
            Ok(d) => d,
            Err(e) => {
                self.client
                    .log_message(MessageType::WARNING, format!("Workspace scan failed: {e}"))
                    .await;
                return;
            }
        };

        // Collect parsed documents without holding the workspace lock.
        let mut parsed: Vec<(Url, std::path::PathBuf, String, Arc<DocumentData>)> = Vec::new();
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(ext.as_str()) {
                continue;
            }
            let Ok(uri) = Url::from_file_path(&path) else { continue };
            let stem = Self::file_stem(&uri);
            match self.read_file(&path).await {
                Ok(src) => parsed.push((uri, path, stem, Arc::new(DocumentData::parse(&src, delimiter)))),
                Err(e) => {
                    self.client
                        .log_message(
                            MessageType::WARNING,
                            format!("Skipping {}: {e}", path.display()),
                        )
                        .await;
                }
            }
        }

        // Retain (uri, stem) pairs before consuming the vec for indexing.
        let uri_stems: Vec<(Url, String)> = parsed
            .iter()
            .map(|(uri, _, stem, _)| (uri.clone(), stem.clone()))
            .collect();

        let count = parsed.len();
        {
            let mut ws = self.workspace.write().await;
            let ref_targets = ws.ref_targets.clone();
            for (uri, path, stem, doc) in parsed {
                ws.symbols.index_document(&uri, &stem, &doc, &ref_targets);
                ws.file_cache.insert(path, doc);
            }
        }

        let t_index = Instant::now();
        self.client
            .log_message(MessageType::INFO, format!("Indexed {count} workspace files."))
            .await;

        // Build workspace snapshot + index once; per-file calls share them via Arc (O(1) clone).
        let t_snapshot_start = Instant::now();
        let shared = if self.plugin_host.is_some() {
            let ws = self.workspace.read().await;
            let snapshot = plugin::build_workspace_snapshot(&ws.open_documents, &ws.file_cache);
            let idx = runtime::build_workspace_index(&ws.open_documents, &ws.file_cache);
            Some((snapshot, idx))
        } else {
            None
        };
        let t_snapshot = t_snapshot_start.elapsed();

        // Publish diagnostics now that the full symbol index is built.
        let mut schema_total = std::time::Duration::ZERO;
        let mut plugin_total = std::time::Duration::ZERO;
        for (uri, stem) in uri_stems {
            let (schema_diags, plugin_data) = {
                let ws = self.workspace.read().await;
                let path = uri.to_file_path().ok();
                match path.as_ref().and_then(|p| ws.file_cache.get(p)) {
                    Some(doc) => {
                        let t = Instant::now();
                        let schema_diags = diagnostics::validate_document(
                            &stem, doc, ws.schema.as_deref(), &ws.symbols,
                        );
                        schema_total += t.elapsed();
                        let plugin_data = shared.as_ref().map(|(snap, idx)| {
                            let ctx = plugin::build_context(&stem, doc);
                            (ctx, idx.clone(), snap.clone())
                        });
                        (schema_diags, plugin_data)
                    }
                    None => (vec![], None),
                }
            };
            let t = Instant::now();
            let plugin_diags = match (plugin_data, &self.plugin_host) {
                (Some((ctx, idx, snap)), Some(ph)) => ph.run(ctx, idx, snap).await,
                _ => vec![],
            };
            plugin_total += t.elapsed();
            let mut diags = schema_diags;
            diags.extend(plugin_diags);
            self.client.publish_diagnostics(uri, diags, None).await;
        }

        let total = t_index.elapsed();
        self.client
            .log_message(
                MessageType::LOG,
                format!(
                    "vlsp perf [{count} files]: snapshot={t_snapshot:.0?} \
                     schema={schema_total:.0?} plugins={plugin_total:.0?} total={total:.0?}"
                ),
            )
            .await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> LspResult<InitializeResult> {
        let root_uri = params
            .workspace_folders
            .as_deref()
            .and_then(|f| f.first())
            .map(|f| f.uri.clone())
            .or(params.root_uri);

        if let Some(uri) = root_uri {
            self.workspace.write().await.root_uri = Some(uri);
        }

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        ..Default::default()
                    },
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "vlsp".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        let has_schema = self.settings.schema_path.is_some()
            || !self.settings.schema_variant.is_empty();
        if has_schema {
            let loader = match find_loader(
                &self.settings.schema_loader,
                self.settings.schema_variant.clone(),
                self.settings.plugin_path.clone(),
            ) {
                Ok(l) => l,
                Err(e) => {
                    self.client.log_message(MessageType::ERROR, format!("{e}")).await;
                    return;
                }
            };
            let schema_path = self.settings.schema_path.clone();
            let result = tokio::task::spawn_blocking(move || {
                loader.load(schema_path.as_deref())
            })
            .await;

            match result {
                Ok(Ok(schema)) => {
                    let schema = Arc::new(schema);
                    let ref_targets = schema.reference_targets();
                    {
                        let mut ws = self.workspace.write().await;
                        ws.ref_targets = ref_targets;
                        ws.schema = Some(Arc::clone(&schema));
                    }
                    if let Some(ph) = &self.plugin_host {
                        ph.set_schema(schema).await;
                    }
                    self.client
                        .log_message(MessageType::INFO, "Schema loaded successfully.")
                        .await;
                    self.scan_and_index_workspace().await;
                }
                Ok(Err(e)) => {
                    self.client
                        .log_message(MessageType::ERROR, format!("Schema load failed: {e:#}"))
                        .await;
                }
                Err(e) => {
                    self.client
                        .log_message(MessageType::ERROR, format!("Schema task panicked: {e}"))
                        .await;
                }
            }
        }
    }

    async fn shutdown(&self) -> LspResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let doc = Arc::new(DocumentData::parse(&params.text_document.text, self.settings.delimiter_char()));
        let stem = Self::file_stem(&uri);

        let (schema_diags, plugin_data) = {
            let mut ws = self.workspace.write().await;
            let ref_targets = ws.ref_targets.clone();
            ws.symbols.remove_file(&stem);
            ws.symbols.index_document(&uri, &stem, &doc, &ref_targets);
            let schema_diags = diagnostics::validate_document(
                &stem, &doc, ws.schema.as_deref(), &ws.symbols,
            );
            ws.open_documents.insert(uri.clone(), Arc::clone(&doc));
            let plugin_data = self.plugin_host.as_ref().map(|_| {
                let ctx = plugin::build_context(&stem, &doc);
                let idx = runtime::build_workspace_index(&ws.open_documents, &ws.file_cache);
                let snap = plugin::build_workspace_snapshot(&ws.open_documents, &ws.file_cache);
                (ctx, idx, snap)
            });
            (schema_diags, plugin_data)
        };

        let plugin_diags = match (plugin_data, &self.plugin_host) {
            (Some((ctx, idx, snap)), Some(ph)) => ph.run(ctx, idx, snap).await,
            _ => vec![],
        };
        let mut diags = schema_diags;
        diags.extend(plugin_diags);
        self.client.publish_diagnostics(uri, diags, None).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.last() {
            let uri = params.text_document.uri;
            let doc = Arc::new(DocumentData::parse(&change.text, self.settings.delimiter_char()));
            let stem = Self::file_stem(&uri);

            let (schema_diags, plugin_data) = {
                let mut ws = self.workspace.write().await;
                let ref_targets = ws.ref_targets.clone();
                ws.symbols.remove_file(&stem);
                ws.symbols.index_document(&uri, &stem, &doc, &ref_targets);
                let schema_diags = diagnostics::validate_document(
                    &stem, &doc, ws.schema.as_deref(), &ws.symbols,
                );
                ws.open_documents.insert(uri.clone(), Arc::clone(&doc));
                let plugin_data = self.plugin_host.as_ref().map(|_| {
                    let ctx = plugin::build_context(&stem, &doc);
                    let idx = runtime::build_workspace_index(&ws.open_documents, &ws.file_cache);
                    let snap = plugin::build_workspace_snapshot(&ws.open_documents, &ws.file_cache);
                    (ctx, idx, snap)
                });
                (schema_diags, plugin_data)
            };

            let plugin_diags = match (plugin_data, &self.plugin_host) {
                (Some((ctx, idx, snap)), Some(ph)) => ph.run(ctx, idx, snap).await,
                _ => vec![],
            };
            let mut diags = schema_diags;
            diags.extend(plugin_diags);
            self.client.publish_diagnostics(uri, diags, None).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.workspace.write().await.open_documents.remove(&uri);
        // Clear editor diagnostics; the file-cache copy remains for workspace validation.
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let file_stem = Self::file_stem(uri);

        // Phase 1: extract cell info and attempt schema-based lookup.
        // Build plugin context data only when the schema has no answer and a plugin host exists.
        let (schema_loc, plugin_data) = {
            let ws = self.workspace.read().await;

            let doc = ws.open_documents.get(uri).or_else(|| {
                uri.to_file_path().ok().and_then(|p| ws.file_cache.get(&p))
            });
            let Some(doc) = doc else { return Ok(None); };

            let Some((col_index, cell)) = doc.cell_at(pos.line, pos.character) else {
                return Ok(None);
            };
            let col_name = match doc.headers.get(col_index) {
                Some(h) => h.clone(),
                None => return Ok(None),
            };
            let cell_value = cell.value.clone();

            let ref_target = ws
                .schema
                .as_ref()
                .and_then(|s| s.find_field(&file_stem, &col_name))
                .and_then(|f| f.field_type.as_ref())
                .filter(|ft| ft.type_name == FieldTypeName::Reference)
                .and_then(|ft| ft.file.as_ref().zip(ft.field.as_ref()))
                .map(|(f, c)| (f.to_lowercase(), c.clone()));

            let schema_loc = ref_target
                .as_ref()
                .and_then(|(ref_file, ref_col)| {
                    ws.symbols.lookup(ref_file, ref_col, &cell_value).cloned()
                });

            let plugin_data = if schema_loc.is_none() {
                self.plugin_host.as_ref().map(|_| {
                    let ctx = plugin::build_hover_context(
                        &file_stem, &col_name, &cell_value, pos.line, doc,
                    );
                    let idx = runtime::build_workspace_index(&ws.open_documents, &ws.file_cache);
                    let snap = plugin::build_workspace_snapshot(&ws.open_documents, &ws.file_cache);
                    (ctx, idx, snap)
                })
            } else {
                None
            };

            (schema_loc, plugin_data)
        }; // read lock released

        if let Some(loc) = schema_loc {
            return Ok(Some(GotoDefinitionResponse::Scalar(loc)));
        }

        // Phase 2: try plugin-based goto definition.
        let plugin_target = match (plugin_data, &self.plugin_host) {
            (Some((ctx, idx, snap)), Some(ph)) => ph.goto_definition(ctx, idx, snap).await,
            _ => return Ok(None),
        };

        let Some((target_file, target_col, target_value)) = plugin_target else {
            return Ok(None);
        };

        // Phase 3: resolve the plugin-provided target via the symbol index.
        let ws = self.workspace.read().await;
        Ok(ws
            .symbols
            .lookup(&target_file, &target_col, &target_value)
            .cloned()
            .map(GotoDefinitionResponse::Scalar))
    }

    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let file_stem = Self::file_stem(uri);

        if pos.line == 0 {
            // Header row hover: return the column's schema description.
            let ws = self.workspace.read().await;
            let Some(doc) = ws.open_documents.get(uri) else {
                return Ok(None);
            };
            let Some(col_index) = doc.header_at(pos.character) else {
                return Ok(None);
            };
            let col_name = doc.headers.get(col_index).map(|s| s.as_str()).unwrap_or("unknown");

            // Compute col_start for the range by summing preceding header lengths.
            let col_start = doc.headers[..col_index]
                .iter()
                .map(|h| h.chars().count() as u32 + 1)
                .sum::<u32>();
            let col_len = col_name.chars().count() as u32;

            let description = ws
                .schema
                .as_ref()
                .and_then(|s| s.find_field(&file_stem, col_name))
                .and_then(|f| f.description.as_deref())
                .map(format_description);

            let text = match description {
                Some(desc) => format!("**{col_name}**\n\n{desc}"),
                None => return Ok(None),
            };

            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: text,
                }),
                range: Some(Range {
                    start: Position { line: 0, character: col_start },
                    end: Position { line: 0, character: col_start + col_len },
                }),
            }));
        }

        // Data row hover: return the cell value plus any plugin-provided context.
        // Column documentation is intentionally omitted here — it belongs on the header.
        let (cell_col_start, cell_len, col_name, cell_value, plugin_hover_data) = {
            let ws = self.workspace.read().await;
            let Some(doc) = ws.open_documents.get(uri) else {
                return Ok(None);
            };
            let Some((col_index, cell)) = doc.cell_at(pos.line, pos.character) else {
                return Ok(None);
            };

            let col_name = doc.headers.get(col_index).map(|s| s.as_str()).unwrap_or("unknown").to_string();
            let cell_value = cell.value.clone();
            let cell_col_start = cell.col_start;
            let cell_len = cell.value.chars().count() as u32;

            let plugin_hover_data = self.plugin_host.as_ref().map(|_| {
                let ctx = plugin::build_hover_context(
                    &file_stem, &col_name, &cell_value, pos.line, doc,
                );
                let idx = runtime::build_workspace_index(&ws.open_documents, &ws.file_cache);
                let snap = plugin::build_workspace_snapshot(&ws.open_documents, &ws.file_cache);
                (ctx, idx, snap)
            });

            (cell_col_start, cell_len, col_name, cell_value, plugin_hover_data)
        }; // read lock released here

        let plugin_content = match (plugin_hover_data, &self.plugin_host) {
            (Some((ctx, idx, snap)), Some(ph)) => ph.hover(ctx, idx, snap).await,
            _ => None,
        };

        let combined = match plugin_content {
            Some(extra) if !extra.is_empty() => extra,
            _ if cell_value.is_empty() => return Ok(None),
            _ => cell_value.clone(),
        };

        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: combined,
            }),
            range: Some(Range {
                start: Position { line: pos.line, character: cell_col_start },
                end: Position { line: pos.line, character: cell_col_start + cell_len },
            }),
        }))
    }
}