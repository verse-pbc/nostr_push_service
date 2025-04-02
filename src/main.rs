use std::sync::Arc;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

// Use items from the library crate
use plur_push_service::cleanup_service;
use plur_push_service::config;
use plur_push_service::error::Result;
use plur_push_service::event_handler;
use plur_push_service::nostr_listener;
use plur_push_service::state; // Assuming Result is pub in error mod

use nostr_sdk::prelude::Event; // Keep this specific use

// A simple replacement for TaskTracker since it's not in tokio_util 0.6.10
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

    match signal::ctrl_c().await {
        Ok(()) => tracing::info!("Received shutdown signal"),
        Err(err) => tracing::error!("Failed to listen for shutdown signal: {}", err),
    }

    tracing::info!("Shutting down services...");

    token.cancel();
    tracker.wait().await;

    tracing::info!("Plur Push Service stopped.");
    Ok(())
}
