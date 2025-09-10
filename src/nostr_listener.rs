use crate::{
    error::{Result, ServiceError},
    event_handler::EventContext,
    redis_store,
    state::AppState,
    subscriptions::SubscriptionManager,
};
use nostr_sdk::prelude::*;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

pub struct NostrListener {
    state: Arc<AppState>,
    client: Arc<Client>,
    subscription_manager: Arc<SubscriptionManager>,
    active_subscription_id: Arc<RwLock<Option<SubscriptionId>>>,
}

impl NostrListener {
    pub fn new(state: Arc<AppState>) -> Self {
        let client = state.nip29_client.client();
        let subscription_manager = state.subscription_manager.clone();
        
        Self {
            state,
            client,
            subscription_manager,
            active_subscription_id: Arc::new(RwLock::new(None)),
        }
    }
    
    pub async fn run(
        &self,
        event_tx: Sender<(Box<Event>, EventContext)>,
        token: CancellationToken,
    ) -> Result<()> {
        info!("Starting Nostr listener with subscription management...");
        
        let service_keys = self.state
            .service_keys
            .clone()
            .ok_or_else(|| {
                ServiceError::Internal("Nostr service keys not configured".to_string())
            })?;
        let service_pubkey = service_keys.public_key();
        
        // Ensure the client is connected
        if !self.is_connected().await {
            self.ensure_connected().await?;
        }
        
        // Recover existing subscriptions from Redis
        self.recover_existing_subscriptions().await?;
        
        // Process historical events first
        self.process_historical_events(&event_tx, &service_pubkey, &token).await?;
        
        // Subscribe to live events with dynamic filter management
        self.subscribe_to_live_events(&token).await?;
        
        // Main event loop
        self.process_live_events(event_tx, service_pubkey, token).await?;
        
        info!("Nostr listener shutting down.");
        Ok(())
    }
    
    async fn recover_existing_subscriptions(&self) -> Result<()> {
        info!("Recovering existing subscriptions from Redis...");
        
        // Get all active subscriptions from Redis
        let all_subscriptions = redis_store::get_all_active_subscriptions(&self.state.redis_pool).await?;
        
        if all_subscriptions.is_empty() {
            info!("No existing subscriptions to recover");
            return Ok(());
        }
        
        let mut recovered_filters = 0;
        let mut unique_filters = HashSet::new();
        
        // Process each subscription
        for (key, filter_json) in all_subscriptions {
            // Parse the key format: "subscription:{app}:{pubkey}:{filter_hash}"
            let parts: Vec<&str> = key.split(':').collect();
            if parts.len() < 4 {
                warn!("Invalid subscription key format: {}", key);
                continue;
            }
            
            let app = parts[1].to_string();
            let pubkey_hex = parts[2].to_string();
            
            // Parse the filter
            let filter: Filter = match serde_json::from_value(filter_json.clone()) {
                Ok(f) => f,
                Err(e) => {
                    warn!("Failed to parse filter from Redis: {}", e);
                    continue;
                }
            };
            
            // Add to subscription manager
            let (filter_hash, is_new) = self.subscription_manager
                .add_user_filter(app.clone(), pubkey_hex.clone(), filter.clone())
                .await;
            
            if is_new {
                unique_filters.insert(filter_hash.clone());
                
                // Create relay subscription for this filter
                match self.client.subscribe(filter, None).await {
                    Ok(output) => {
                        let mut user_subs = self.state.user_subscriptions.write().await;
                        user_subs.insert(filter_hash.clone(), output.val.clone());
                        debug!(
                            filter_hash = &filter_hash[..8],
                            subscription_id = %output.val,
                            "Recovered relay subscription"
                        );
                    }
                    Err(e) => {
                        error!(
                            filter_hash = &filter_hash[..8],
                            error = %e,
                            "Failed to create relay subscription during recovery"
                        );
                    }
                }
            }
            
            recovered_filters += 1;
        }
        
        info!(
            "Recovered {} subscriptions with {} unique filters",
            recovered_filters,
            unique_filters.len()
        );
        
        Ok(())
    }
    
    async fn is_connected(&self) -> bool {
        let relays = self.client.relays().await;
        !relays.is_empty() && relays.values().any(|s| s.is_connected())
    }
    
    async fn ensure_connected(&self) -> Result<()> {
        warn!("Nostr client not connected. Attempting to connect...");
        let relay_url = &self.state.settings.nostr.relay_url;
        
        if relay_url.is_empty() {
            return Err(ServiceError::Internal(
                "Nostr relay URL missing in settings".to_string(),
            ));
        }
        
        self.client.add_relay(relay_url.as_str()).await?;
        self.client.connect().await;
        
        if !self.is_connected().await {
            return Err(ServiceError::Internal(
                "Failed to connect to Nostr relay".to_string(),
            ));
        }
        
        info!("Successfully connected to Nostr relay");
        Ok(())
    }
    
    async fn process_historical_events(
        &self,
        event_tx: &Sender<(Box<Event>, EventContext)>,
        service_pubkey: &PublicKey,
        token: &CancellationToken,
    ) -> Result<()> {
        let process_window_duration = Duration::from_secs(
            self.state.settings.service.process_window_days as u64 * 24 * 60 * 60
        );
        let since_timestamp = Timestamp::now() - process_window_duration;
        
        // Build filter for control kinds only
        let control_kinds: Vec<Kind> = self.state.settings.service.control_kinds
            .iter()
            .filter_map(|&k| u16::try_from(k).ok())
            .map(Kind::from)
            .collect();
        
        let filter = Filter::new()
            .kinds(control_kinds)
            .since(since_timestamp);
        
        info!(since = %since_timestamp, "Querying historical control events...");
        
        tokio::select! {
            biased;
            _ = token.cancelled() => {
                info!("Cancelled before historical event query");
                return Ok(());
            }
            fetch_result = self.client.fetch_events(filter, Duration::from_secs(60)) => {
                match fetch_result {
                    Ok(historical_events) => {
                        info!(count = historical_events.len(), "Processing historical control events...");
                        
                        for event in historical_events {
                            if event.pubkey == *service_pubkey {
                                continue;
                            }
                            
                            tokio::select! {
                                biased;
                                _ = token.cancelled() => {
                                    info!("Cancelled during historical processing");
                                    return Ok(());
                                }
                                send_res = event_tx.send((Box::new(event), EventContext::Historical)) => {
                                    if let Err(e) = send_res {
                                        error!("Failed to send historical event: {}", e);
                                        return Err(ServiceError::Internal(
                                            "Event handler channel closed".to_string()
                                        ));
                                    }
                                }
                            }
                        }
                        
                        info!("Finished processing historical control events");
                    }
                    Err(e) => {
                        error!("Failed to query historical control events: {}", e);
                        warn!("Proceeding without historical events");
                    }
                }
            }
        }
        
        Ok(())
    }
    
    async fn subscribe_to_live_events(&self, token: &CancellationToken) -> Result<()> {
        // Subscribe to control kinds and DM kinds for live events
        // User subscriptions are managed directly by event handlers
        let control_kinds: Vec<Kind> = self.state.settings.service.control_kinds
            .iter()
            .filter_map(|&k| u16::try_from(k).ok())
            .map(Kind::from)
            .collect();
            
        let dm_kinds: Vec<Kind> = self.state.settings.service.dm_kinds
            .iter()
            .filter_map(|&k| u16::try_from(k).ok())
            .map(Kind::from)
            .collect();
        
        // Combine control and DM kinds
        let mut all_kinds = control_kinds;
        all_kinds.extend(dm_kinds);
        
        // Look back 2 days to catch both control events and NIP-17 DMs
        // NIP-17 spec: "randomize created_at in up to two days in the past"
        let since = Timestamp::now() - Duration::from_secs(2 * 24 * 60 * 60); // 2 days
        
        // Use a single filter with all kinds and 2-day lookback
        let filter = Filter::new()
            .kinds(all_kinds.clone())
            .since(since);
        
        info!("Subscribing to control kinds and DM kinds for live events... kinds: {:?}", 
              all_kinds.iter().map(|k| k.as_u16()).collect::<Vec<_>>());
        
        tokio::select! {
            biased;
            _ = token.cancelled() => {
                info!("Cancelled before live subscription");
                return Ok(());
            }
            sub_result = self.client.subscribe(filter, None) => {
                match sub_result {
                    Ok(output) => {
                        let mut active_sub = self.active_subscription_id.write().await;
                        *active_sub = Some(output.val);
                        info!("Successfully subscribed to control kinds and DM kinds");
                    }
                    Err(e) => {
                        error!("Failed to subscribe to control kinds and DM kinds: {}", e);
                        return Err(e.into());
                    }
                }
            }
        }
        
        Ok(())
    }
    
    async fn process_live_events(
        &self,
        event_tx: Sender<(Box<Event>, EventContext)>,
        service_pubkey: PublicKey,
        token: CancellationToken,
    ) -> Result<()> {
        let mut notifications = self.client.notifications();
        
        info!("Processing live events...");
        
        loop {
            tokio::select! {
                biased;
                _ = token.cancelled() => {
                    info!("Cancellation received, shutting down");
                    break;
                }
                
                res = notifications.recv() => {
                    match res {
                        Ok(notification) => {
                            match notification {
                                RelayPoolNotification::Event { event, .. } => {
                                    if event.pubkey == service_pubkey {
                                        debug!("Skipping event from service account");
                                        continue;
                                    }
                                    
                                    let event_id = event.id;
                                    let event_kind = event.kind;
                                    
                                    debug!(event_id = %event_id, kind = %event_kind, "Received live event");
                                    
                                    tokio::select! {
                                        biased;
                                        _ = token.cancelled() => {
                                            info!("Cancelled while sending event");
                                            break;
                                        }
                                        send_res = event_tx.send((event, EventContext::Live)) => {
                                            if let Err(e) = send_res {
                                                error!("Failed to send live event: {}", e);
                                                break;
                                            }
                                        }
                                    }
                                }
                                RelayPoolNotification::Message { relay_url, message } => {
                                    debug!(%relay_url, ?message, "Received relay message");
                                }
                                RelayPoolNotification::Shutdown => {
                                    info!("Received shutdown notification");
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            error!("Error receiving notification: {}", e);
                            break;
                        }
                    }
                }
            }
        }
        
        Ok(())
    }
}