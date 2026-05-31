use std::sync::Arc;
use tokio::sync::RwLock;
use tower_lsp::lsp_types::*;
use tower_lsp::{jsonrpc::Result as LspResult, Client, LanguageServer};

use crate::diagnostics;
use crate::document::DocumentData;
use crate::plugin;
use crate::runtime::ScriptRuntime;
use crate::schema::{format_description, load_schema, FieldTypeName};
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
        let mut parsed: Vec<(Url, std::path::PathBuf, String, DocumentData)> = Vec::new();
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(ext.as_str()) {
                continue;
            }
            let Ok(uri) = Url::from_file_path(&path) else { continue };
            let stem = Self::file_stem(&uri);
            match self.read_file(&path).await {
                Ok(src) => parsed.push((uri, path, stem, DocumentData::parse(&src, delimiter))),
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

        self.client
            .log_message(MessageType::INFO, format!("Indexed {count} workspace files."))
            .await;

        // Publish diagnostics now that the full symbol index is built.
        for (uri, stem) in uri_stems {
            let (schema_diags, plugin_ctx) = {
                let ws = self.workspace.read().await;
                let path = uri.to_file_path().ok();
                match path.as_ref().and_then(|p| ws.file_cache.get(p)) {
                    Some(doc) => {
                        let schema_diags = diagnostics::validate_document(
                            &stem, doc, ws.schema.as_ref(), &ws.symbols,
                        );
                        let plugin_ctx = self.plugin_host.as_ref().map(|_| {
                            let ws_json = plugin::workspace_to_json(
                                &ws.open_documents, &ws.file_cache,
                            );
                            plugin::build_context(&stem, doc, ws_json)
                        });
                        (schema_diags, plugin_ctx)
                    }
                    None => (vec![], None),
                }
            };
            let plugin_diags = match (plugin_ctx, &self.plugin_host) {
                (Some(ctx), Some(ph)) => ph.run(ctx).await,
                _ => vec![],
            };
            let mut diags = schema_diags;
            diags.extend(plugin_diags);
            self.client.publish_diagnostics(uri, diags, None).await;
        }
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
        if let Some(path) = self.settings.schema_path.clone() {
            let result = tokio::task::spawn_blocking(move || {
                ScriptRuntime::new().and_then(|mut rt| load_schema(&mut rt, &path))
            })
            .await;

            match result {
                Ok(Ok(schema)) => {
                    let ref_targets = schema.reference_targets();
                    {
                        let mut ws = self.workspace.write().await;
                        ws.ref_targets = ref_targets;
                        ws.schema = Some(schema);
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
        let doc = DocumentData::parse(&params.text_document.text, self.settings.delimiter_char());
        let stem = Self::file_stem(&uri);

        let (schema_diags, plugin_ctx) = {
            let mut ws = self.workspace.write().await;
            let ref_targets = ws.ref_targets.clone();
            ws.symbols.remove_file(&stem);
            ws.symbols.index_document(&uri, &stem, &doc, &ref_targets);
            let schema_diags = diagnostics::validate_document(
                &stem, &doc, ws.schema.as_ref(), &ws.symbols,
            );
            ws.open_documents.insert(uri.clone(), doc);
            let plugin_ctx = self.plugin_host.as_ref().map(|_| {
                let doc_ref = ws.open_documents.get(&uri).unwrap();
                let ws_json = plugin::workspace_to_json(&ws.open_documents, &ws.file_cache);
                plugin::build_context(&stem, doc_ref, ws_json)
            });
            (schema_diags, plugin_ctx)
        };

        let plugin_diags = match (plugin_ctx, &self.plugin_host) {
            (Some(ctx), Some(ph)) => ph.run(ctx).await,
            _ => vec![],
        };
        let mut diags = schema_diags;
        diags.extend(plugin_diags);
        self.client.publish_diagnostics(uri, diags, None).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.last() {
            let uri = params.text_document.uri;
            let doc = DocumentData::parse(&change.text, self.settings.delimiter_char());
            let stem = Self::file_stem(&uri);

            let (schema_diags, plugin_ctx) = {
                let mut ws = self.workspace.write().await;
                let ref_targets = ws.ref_targets.clone();
                ws.symbols.remove_file(&stem);
                ws.symbols.index_document(&uri, &stem, &doc, &ref_targets);
                let schema_diags = diagnostics::validate_document(
                    &stem, &doc, ws.schema.as_ref(), &ws.symbols,
                );
                ws.open_documents.insert(uri.clone(), doc);
                let plugin_ctx = self.plugin_host.as_ref().map(|_| {
                    let doc_ref = ws.open_documents.get(&uri).unwrap();
                    let ws_json = plugin::workspace_to_json(&ws.open_documents, &ws.file_cache);
                    plugin::build_context(&stem, doc_ref, ws_json)
                });
                (schema_diags, plugin_ctx)
            };

            let plugin_diags = match (plugin_ctx, &self.plugin_host) {
                (Some(ctx), Some(ph)) => ph.run(ctx).await,
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

        let ws = self.workspace.read().await;

        let doc = ws.open_documents.get(uri).or_else(|| {
            uri.to_file_path().ok().and_then(|p| ws.file_cache.get(&p))
        });
        let Some(doc) = doc else { return Ok(None); };

        let Some((col_index, cell)) = doc.cell_at(pos.line, pos.character) else {
            return Ok(None);
        };
        let col_name = match doc.headers.get(col_index) {
            Some(h) => h.as_str(),
            None => return Ok(None),
        };
        let cell_value = cell.value.clone();
        let file_stem = Self::file_stem(uri);

        let ref_target = ws
            .schema
            .as_ref()
            .and_then(|s| s.find_field(&file_stem, col_name))
            .and_then(|f| f.field_type.as_ref())
            .filter(|ft| ft.type_name == FieldTypeName::Reference)
            .and_then(|ft| ft.file.as_ref().zip(ft.field.as_ref()))
            .map(|(f, c)| (f.to_lowercase(), c.clone()));

        let Some((ref_file, ref_col)) = ref_target else { return Ok(None); };

        Ok(ws
            .symbols
            .lookup(&ref_file, &ref_col, &cell_value)
            .cloned()
            .map(GotoDefinitionResponse::Scalar))
    }

    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let file_stem = Self::file_stem(uri);

        // Collect everything we need from the workspace while holding the read
        // lock, then drop it before the async plugin call.
        let (cell_col_start, cell_len, schema_content, plugin_hover_ctx) = {
            let ws = self.workspace.read().await;
            let Some(doc) = ws.open_documents.get(uri) else {
                return Ok(None);
            };
            let Some((col_index, cell)) = doc.cell_at(pos.line, pos.character) else {
                return Ok(None);
            };

            let col_name = doc.headers.get(col_index).map(|s| s.as_str()).unwrap_or("unknown");
            let cell_value = cell.value.clone();
            let cell_col_start = cell.col_start;
            let cell_len = cell.value.chars().count() as u32;

            let description = ws
                .schema
                .as_ref()
                .and_then(|s| s.find_field(&file_stem, col_name))
                .and_then(|f| f.description.as_deref())
                .map(format_description);

            let schema_content = match description {
                Some(desc) => format!("**{col_name}**\n\n{desc}"),
                None => format!("**{col_name}**\n\nValue: `{cell_value}`"),
            };

            let plugin_hover_ctx = self.plugin_host.as_ref().map(|_| {
                let ws_json = plugin::workspace_to_json(&ws.open_documents, &ws.file_cache);
                plugin::build_hover_context(
                    &file_stem, col_name, &cell_value, pos.line, doc, ws_json,
                )
            });

            (cell_col_start, cell_len, schema_content, plugin_hover_ctx)
        }; // read lock released here

        let plugin_content = match (plugin_hover_ctx, &self.plugin_host) {
            (Some(ctx), Some(ph)) => ph.hover(ctx).await,
            _ => None,
        };

        let combined = match plugin_content {
            Some(extra) => format!("{schema_content}\n\n---\n\n{extra}"),
            None => schema_content,
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