use axum::{
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use plur_push_service::cleanup_service;
use plur_push_service::config;
use plur_push_service::error::Result;
use plur_push_service::event_handler;
use plur_push_service::nostr_listener;
use plur_push_service::state; // Assuming Result is pub in error mod

use nostr_sdk::prelude::Event; // Keep this specific use

// Reintroduce SimpleTaskTracker
// NOTE: We are using this custom tracker because the standard
// `tokio_util::task::TaskTracker` requires tokio-util >= 0.7,
// but the `firebase-messaging-rs` git dependency currently pulls
// in `tokio-util` 0.6.x, causing a version conflict.
// Consider forking `firebase-messaging-rs` or finding an alternative
// FCM crate to resolve this properly and use `TaskTracker`.
struct SimpleTaskTracker {
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl SimpleTaskTracker {
    fn new() -> Self {
        Self {
            handles: Vec::new(),
        }
    }

    fn spawn<F>(&mut self, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.handles.push(tokio::spawn(future));
    }

    async fn wait(self) {
        for handle in self.handles {
            let _ = handle.await;
        }
    }
}

async fn health_check() -> (StatusCode, &'static str) {
    (StatusCode::OK, "OK")
}

async fn serve_frontend() -> impl IntoResponse {
    const HTML: &str = include_str!("../frontend/index.html");
    Html(HTML)
}

async fn serve_firebase_config() -> impl IntoResponse {
    const JS: &str = include_str!("../frontend/firebase-config.js");
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        JS,
    )
}

async fn serve_service_worker() -> impl IntoResponse {
    const JS: &str = include_str!("../frontend/firebase-messaging-sw.js");
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        JS,
    )
}

async fn run_server(app_state: Arc<state::AppState>, token: CancellationToken) {
    let app = Router::new()
        .route("/", get(serve_frontend))
        .route("/firebase-config.js", get(serve_firebase_config))
        .route("/firebase-messaging-sw.js", get(serve_service_worker))
        .route("/health", get(health_check));

    let listen_addr_str = &app_state.settings.server.listen_addr;
    let addr: SocketAddr = match listen_addr_str.parse() {
        Ok(addr) => addr,
        Err(e) => {
            tracing::error!(
                "Invalid server.listen_addr '{}': {}. Exiting server task.",
                listen_addr_str,
                e
            );
            return;
        }
    };

    tracing::info!("HTTP server listening on {}", addr);

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!("Failed to bind HTTP server: {}", e);
            return;
        }
    };

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            token.cancelled().await;
            tracing::info!("HTTP server shutting down.");
        })
        .await
    {
        tracing::error!("HTTP server error: {}", e);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(true)
        .init();

    tracing::info!("Starting Plur Push Service...");

    let settings = config::Settings::new()?;
    tracing::info!("Configuration loaded successfully");

    let app_state = Arc::new(state::AppState::new(settings).await?);
    tracing::info!("Application state initialized (Redis Pool, FCM Client)");

    let mut tracker = SimpleTaskTracker::new();
    let token = CancellationToken::new();

    let (nostr_event_tx, nostr_event_rx) = tokio::sync::mpsc::channel::<Box<Event>>(1000);

    let state_nostr = Arc::clone(&app_state);
    let token_nostr = token.clone();
    let tx_nostr = nostr_event_tx.clone();
    tracker.spawn(async move {
        if let Err(e) = nostr_listener::run(state_nostr, tx_nostr, token_nostr).await {
            tracing::error!("Nostr listener failed: {}", e);
        }
        tracing::info!("Nostr listener task finished.");
    });
    tracing::info!("Nostr listener started");

    let state_event = Arc::clone(&app_state);
    let token_event = token.clone();
    tracker.spawn(async move {
        if let Err(e) = event_handler::run(state_event, nostr_event_rx, token_event).await {
            tracing::error!("Event handler failed: {}", e);
        }
        tracing::info!("Event handler task finished.");
    });
    tracing::info!("Event handler started");

    let state_cleanup = Arc::clone(&app_state);
    let token_cleanup = token.clone();
    tracker.spawn(async move {
        if let Err(e) = cleanup_service::run_cleanup_service(state_cleanup, token_cleanup).await {
            tracing::error!("Cleanup service failed: {}", e);
        }
        tracing::info!("Cleanup service task finished.");
    });
    tracing::info!("Cleanup service started");

    let token_server = token.clone();
    let state_server = Arc::clone(&app_state);
    tracker.spawn(async move {
        run_server(state_server, token_server).await;
        tracing::info!("HTTP server task finished.");
    });
    tracing::info!("HTTP server started");

    match signal::ctrl_c().await {
        Ok(()) => tracing::info!("Received shutdown signal"),
        Err(err) => tracing::error!("Failed to listen for shutdown signal: {}", err),
    }

    tracing::info!("Shutting down services...");

    token.cancel();

    // Wait for all tracked tasks to complete using SimpleTaskTracker's wait
    tracker.wait().await;

    tracing::info!("Plur Push Service stopped.");
    Ok(())
}
