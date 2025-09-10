use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Json},
    routing::get,
    Router,
};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use nostr_push_service::cleanup_service;
use nostr_push_service::config;
use nostr_push_service::error::Result;
use nostr_push_service::event_handler;
use nostr_push_service::nostr_listener;
use nostr_push_service::state; // Assuming Result is pub in error mod

use nostr_sdk::prelude::Event; // Keep this specific use
use nostr_sdk::ToBech32;

// Which app's frontend config to serve for the demo frontend
// Can be overridden with FRONTEND_APP environment variable
fn get_frontend_app() -> String {
    std::env::var("FRONTEND_APP").unwrap_or_else(|_| "nostrpushdemo".to_string())
}

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
    
    // Get the service public key from configuration
    let service_npub = if let Ok(private_key_hex) = std::env::var("NOSTR_PUSH__SERVICE__PRIVATE_KEY_HEX") {
        if let Ok(secret_key) = nostr_sdk::prelude::SecretKey::from_hex(&private_key_hex) {
            let keys = nostr_sdk::prelude::Keys::new(secret_key);
            keys.public_key().to_bech32().unwrap_or_else(|_| "".to_string())
        } else {
            "".to_string()
        }
    } else {
        "".to_string()
    };
    
    // Inject the service pubkey into the HTML
    let html_with_config = HTML.replace(
        "const SERVICE_NPUB = 'npub1mutnyacc9uc4t5mmxvpprwsauj5p2qxq95v4a9j0jxl8wnkfvuyqpq9mhx';",
        &format!("const SERVICE_NPUB = '{}';", service_npub)
    );
    
    Html(html_with_config)
}

async fn serve_firebase_config(state: Arc<state::AppState>) -> impl IntoResponse {
    // Get the selected frontend app from environment
    let frontend_app = get_frontend_app();
    
    // Find the app config for the frontend app
    let app_config = state.settings.apps.iter()
        .find(|app| app.name == frontend_app);
    
    let config = match app_config {
        Some(app) => {
            format!(
                r#"// Firebase configuration for Web Push
const firebaseConfig = {{
    apiKey: "{}",
    authDomain: "{}",
    projectId: "{}",
    storageBucket: "{}",
    messagingSenderId: "{}",
    appId: "{}"
}};"#,
                app.frontend_config.api_key,
                app.frontend_config.auth_domain,
                app.frontend_config.project_id,
                app.frontend_config.storage_bucket,
                app.frontend_config.messaging_sender_id,
                app.frontend_config.app_id
            )
        },
        None => {
            // Return empty config if app not found
            r#"// Firebase configuration not found
const firebaseConfig = {};"#.to_string()
        }
    };
    
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        config,
    )
}

async fn serve_service_worker() -> impl IntoResponse {
    // Serve the service worker as-is, it will load config from /firebase-config.js
    let sw_content = include_str!("../frontend/firebase-messaging-sw.js");
    
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        sw_content,
    )
}

async fn serve_fcm_config(state: Arc<state::AppState>) -> impl IntoResponse {
    // Serve the frontend config for the demo app specified by FRONTEND_APP env var
    // This endpoint is only for the demo frontend - real apps would have their own config
    
    let frontend_app = get_frontend_app();
    
    // Find the app config for the frontend app
    let app_config = state.settings.apps.iter()
        .find(|app| app.name == frontend_app);
    
    match app_config {
        Some(app) => {
            let config = json!({
                "appName": app.name,  // Include the app name for frontend to use in tags
                "apiKey": app.frontend_config.api_key,
                "authDomain": app.frontend_config.auth_domain,
                "projectId": app.frontend_config.project_id,
                "storageBucket": app.frontend_config.storage_bucket,
                "messagingSenderId": app.frontend_config.messaging_sender_id,
                "appId": app.frontend_config.app_id,
                "measurementId": app.frontend_config.measurement_id,
                "vapidPublicKey": app.frontend_config.vapid_public_key
            });
            
            tracing::info!("Serving FCM config for demo frontend app: {}", frontend_app);
            Json(config)
        },
        None => {
            tracing::error!("Frontend app '{}' not found in configuration", frontend_app);
            // Return empty config if app not found
            Json(json!({}))
        }
    }
}

// Removed serve_nostr_bundle - using CDN version for demo

async fn serve_manifest() -> impl IntoResponse {
    const MANIFEST: &str = include_str!("../frontend/manifest.json");
    (
        StatusCode::OK,
        [("Content-Type", "application/manifest+json")],
        MANIFEST
    )
}

async fn serve_icon_192() -> impl IntoResponse {
    const ICON: &[u8] = include_bytes!("../frontend/icon-192x192.png");
    (
        StatusCode::OK,
        [("Content-Type", "image/png")],
        ICON
    )
}

async fn serve_icon_512() -> impl IntoResponse {
    const ICON: &[u8] = include_bytes!("../frontend/icon-512x512.png");
    (
        StatusCode::OK,
        [("Content-Type", "image/png")],
        ICON
    )
}

async fn run_server(app_state: Arc<state::AppState>, token: CancellationToken) {
    let app_state_clone = app_state.clone();
    let app_state_clone2 = app_state.clone();
    
    let app = Router::new()
        .route("/", get(serve_frontend))
        .route("/firebase-config.js", get(move || serve_firebase_config(app_state_clone2.clone())))
        .route("/firebase-messaging-sw.js", get(serve_service_worker))
        .route("/config/fcm.json", get(move || serve_fcm_config(app_state_clone.clone())))
        // nostr.bundle.js now served from CDN
        .route("/manifest.json", get(serve_manifest))
        .route("/icon-192x192.png", get(serve_icon_192))
        .route("/icon-512x512.png", get(serve_icon_512))
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
            token.cancel(); // Cancel all other tasks
            return;
        }
    };

    tracing::info!("HTTP server listening on {}", addr);

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!("Failed to bind HTTP server: {}", e);
            tracing::error!("Cancelling token to trigger shutdown...");
            token.cancel(); // Cancel all other tasks when bind fails
            return;
        }
    };

    let shutdown_token = token.clone();
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_token.cancelled().await;
            tracing::info!("HTTP server shutting down.");
        })
        .await
    {
        tracing::error!("HTTP server error: {}", e);
        token.cancel(); // Cancel all other tasks on server error
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(true)
        .init();

    tracing::info!("Starting Nostr Push Service...");

    let settings = config::Settings::new()?;
    tracing::info!("Configuration loaded successfully");

    let app_state = Arc::new(state::AppState::new(settings).await?);
    tracing::info!("Application state initialized (Redis Pool, FCM Client)");

    let mut tracker = SimpleTaskTracker::new();
    let token = CancellationToken::new();

    let (nostr_event_tx, nostr_event_rx) = tokio::sync::mpsc::channel::<(Box<Event>, event_handler::EventContext)>(1000);

    let state_nostr = Arc::clone(&app_state);
    let token_nostr = token.clone();
    let tx_nostr = nostr_event_tx.clone();
    tracker.spawn(async move {
        let listener = nostr_listener::NostrListener::new(state_nostr);
        if let Err(e) = listener.run(tx_nostr, token_nostr).await {
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

    // Create a future for token cancellation that we can poll
    let token_cancelled = token.child_token();
    
    // Wait for either Ctrl+C or cancellation token (from HTTP server failure)
    tokio::select! {
        _ = signal::ctrl_c() => {
            tracing::info!("Received shutdown signal");
        }
        _ = token_cancelled.cancelled() => {
            tracing::info!("Shutdown triggered by task failure");
        }
    }

    tracing::info!("Shutting down services...");

    token.cancel();

    // Wait for all tracked tasks to complete using SimpleTaskTracker's wait
    tracker.wait().await;

    tracing::info!("Nostr Push Service stopped.");
    Ok(())
}
