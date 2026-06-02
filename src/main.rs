mod backend;
mod cli;
mod diagnostics;
mod document;
mod plugin;
mod runtime;
mod schema;
mod settings;
mod workspace;

use std::sync::Arc;

use clap::Parser;
use config::{Config, Environment, File};
use tokio::sync::RwLock;
use tower_lsp::{LspService, Server};

use cli::CliArgs;
use settings::{IoType, VectorLspSettings};
use workspace::Workspace;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = CliArgs::parse();

    let raw = Config::builder()
        .add_source(File::with_name(&args.config_file).required(false))
        .add_source(Environment::with_prefix("VLSP"))
        .build()?;

    let settings = Arc::new(
        raw.try_deserialize::<VectorLspSettings>().unwrap_or_default(),
    );
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
