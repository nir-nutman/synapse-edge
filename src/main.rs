mod proxy;
mod triage;

use std::env;
use std::net::SocketAddr;

use axum::routing::post;
use axum::Router;
use dotenvy::dotenv;
use reqwest::Client;
use tokio::net::TcpListener;
use tracing::info;
use tracing::Level;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use proxy::{chat_completions, AppState, OPENAI_CHAT_PATH};

#[tokio::main]
async fn main() {
    let _ = dotenv();

    // Write logs to ./logs/synapse-edge.log (rotated daily) AND to the terminal.
    // The `_log_guard` must stay alive for the whole program — dropping it early
    // would stop the background thread that flushes logs to disk.
    let file_appender = tracing_appender::rolling::daily("logs", "synapse-edge.log");
    let (file_writer, _log_guard) = tracing_appender::non_blocking(file_appender);

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .compact();
    let file_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_ansi(false) // no color codes in the log file
        .with_writer(file_writer);

    tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::from_level(
            Level::INFO,
        ))
        .with(stdout_layer)
        .with(file_layer)
        .init();

    let local_url = env::var("LOCAL_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    let cloud_url = env::var("CLOUD_URL").unwrap_or_else(|_| "https://api.openai.com".to_string());
    let cloud_api_key = env::var("CLOUD_API_KEY")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let bind_addr = env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    let state = AppState {
        client: Client::new(),
        local_url,
        cloud_url,
        cloud_api_key,
    };

    let app = Router::new()
        .route(OPENAI_CHAT_PATH, post(chat_completions))
        .with_state(state);

    let addr: SocketAddr = bind_addr
        .parse()
        .unwrap_or_else(|_| "0.0.0.0:3000".parse().expect("valid default bind address"));

    info!(%addr, "Synapse-Edge Phase 1 proxy started");

    let listener = TcpListener::bind(addr)
        .await
        .expect("failed to bind TCP listener");
    axum::serve(listener, app)
        .await
        .expect("server exited unexpectedly");
}
