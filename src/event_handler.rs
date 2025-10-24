use crate::{
    crypto::CryptoService,
    error::Result,
    fcm_sender,
    handlers::CommunityHandler,
    models::FcmPayload,
    nostr::nip29,
    redis_store,
    state::AppState,
    subscriptions::SubscriptionManager,
};
use nostr_sdk::prelude::*;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, trace, warn};

/// Context for event processing to distinguish historical from live events
#[derive(Debug, Clone, Copy)]
pub enum EventContext {
    /// Historical event being replayed (e.g., during startup or reconnection)
    Historical,
    /// Live event received in real-time
    Live,
}

const KIND_REGISTRATION: Kind = Kind::Custom(3079);
const KIND_DEREGISTRATION: Kind = Kind::Custom(3080);
const KIND_SUBSCRIPTION_UPSERT: Kind = Kind::Custom(3081);
const KIND_SUBSCRIPTION_DELETE: Kind = Kind::Custom(3082);
const KIND_NIP44_DM: Kind = Kind::GiftWrap; // NIP-44 DM (kind 1059)
const KIND_NIP17_DM: Kind = Kind::PrivateDirectMessage; // NIP-17 Private DM (kind 14)
const BROADCASTABLE_EVENT_KINDS: [Kind; 1] = [Kind::Custom(9)]; // Only kind 9 is broadcastable

// Replay horizon: ignore events older than this
const REPLAY_HORIZON_DAYS: u64 = 7;

/// Check if event is targeted to this service via p tag
fn is_event_for_service(event: &Event, service_pubkey: &PublicKey) -> bool {
    event.tags
        .iter()
        .filter(|t| t.kind() == TagKind::p())
        .filter_map(|t| t.content())
        .any(|pubkey_str| {
            PublicKey::from_str(pubkey_str)
                .map(|pk| pk == *service_pubkey)
                .unwrap_or(false)
        })
}

/// Extract app tag from event
fn get_app_tag(event: &Event) -> Option<String> {
    event.tags
        .iter()
        .find(|t| t.kind() == TagKind::custom("app"))
        .and_then(|t| t.content())
        .map(|s| s.to_string())
}

/// Check if event is a DM
fn is_dm_event(event: &Event) -> bool {
    event.kind == KIND_NIP44_DM || event.kind == KIND_NIP17_DM
}

pub async fn run(
    state: Arc<AppState>,
    mut event_rx: Receiver<(Box<Event>, EventContext)>,
    token: CancellationToken,
) -> Result<()> {
    info!("Starting event handler with NIP-72 support...");

    let relay_url = &state.settings.nostr.relay_url;

    loop {
        tokio::select! {
            biased;
            _ = token.cancelled() => {
                info!("Event handler cancellation received. Shutting down...");
                break;
            }

            maybe_event = event_rx.recv() => {
                let Some((event, context)) = maybe_event else {
                    info!("Event channel closed. Event handler shutting down.");
                    break;
                };

                let event_id = event.id;
                let event_kind = event.kind;
                let pubkey = event.pubkey;

                debug!(event_id = %event_id, kind = %event_kind, pubkey = %pubkey, context = ?context, "Event handler received event");

                // Check replay horizon - ignore events that are too old
                if is_event_too_old(&event) {
                    debug!(event_id = %event_id, created_at = %event.created_at, "Ignoring old event beyond replay horizon");
                    continue;
                }

                // Check if already processed
                tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        info!("Event handler cancelled while checking if event {} was processed.", event_id);
                        break;
                    }
                    processed_result = redis_store::is_event_processed(&state.redis_pool, &event_id) => {
                        match processed_result {
                            Ok(true) => {
                                trace!(event_id = %event_id, "Skipping already processed event");
                                continue;
                            }
                            Ok(false) => {
                                // Not processed, continue handling
                            }
                            Err(e) => {
                                error!(event_id = %event_id, error = %e, "Failed to check if event was processed");
                                continue;
                            }
                        }
                    }
                }

                // Route the event based on its type
                let handler_result = route_event(&state, &event, context, relay_url, token.clone()).await;

                match handler_result {
                    Ok(_) => {
                        trace!(event_id = %event_id, kind = %event_kind, "Handler finished successfully");
                        tokio::select! {
                            biased;
                            _ = token.cancelled() => {
                                info!("Event handler cancelled before marking event {} as processed.", event_id);
                                break;
                            }
                            mark_result = redis_store::mark_event_processed(
                                &state.redis_pool,
                                &event_id,
                                state.settings.service.processed_event_ttl_secs,
                            ) => {
                                if let Err(e) = mark_result {
                                    error!(event_id = %event_id, error = %e, "Failed to mark event as processed");
                                } else {
                                    debug!(event_id = %event_id, "Successfully marked event as processed");
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!(event_id = %event_id, error = %e, "Failed to handle event");
                        // Continue processing other events
                    }
                }

                if token.is_cancelled() {
                    info!(event_id = %event_id, "Event handler cancellation detected after processing event.");
                    break;
                }
            }
        }
    }

    info!("Event handler shut down.");
    Ok(())
}

async fn route_event(
    state: &AppState,
    event: &Event,
    context: EventContext,
    relay_url: &str,
    token: CancellationToken,
) -> Result<()> {
    let event_kind = event.kind;
    let event_id = event.id;
    
    // Check for push notification management events (3079-3082)
    let is_control_event = event_kind == KIND_REGISTRATION 
        || event_kind == KIND_DEREGISTRATION 
        || event_kind == KIND_SUBSCRIPTION_UPSERT 
        || event_kind == KIND_SUBSCRIPTION_DELETE;
    
    if is_control_event {
        debug!(event_id = %event_id, kind = %event_kind, "Processing control event");
        
        // Check if this event is targeted to our service via p tag
        if let Some(ref service_keys) = state.service_keys {
            if !is_event_for_service(event, &service_keys.public_key()) {
                debug!(event_id = %event_id, kind = %event_kind, "Ignoring control event not targeted to our service");
                return Ok(());
            }
            debug!(event_id = %event_id, "Control event is for this service");
        } else {
            warn!("No service keys configured - cannot filter by p tag");
        }
        
        // Check if the app tag is in our supported apps list
        let app = get_app_tag(event).unwrap_or_else(|| "default".to_string());
        if !state.supported_apps.contains(&app) {
            debug!(
                event_id = %event_id,
                kind = %event_kind,
                app = %app,
                "Ignoring control event for unconfigured app"
            );
            return Ok(());
        }
        
        // Route to appropriate control handler
        if event_kind == KIND_REGISTRATION {
            return handle_registration(state, event).await;
        } else if event_kind == KIND_DEREGISTRATION {
            return handle_deregistration(state, event).await;
        } else if event_kind == KIND_SUBSCRIPTION_UPSERT {
            return handle_subscription_upsert(state, event, token).await;
        } else if event_kind == KIND_SUBSCRIPTION_DELETE {
            return handle_subscription_delete(state, event, token).await;
        } else {
            return Ok(());
        }
    }
    
    // Check if it's a DM
    if is_dm_event(event) {
        info!(event_id = %event_id, kind = %event_kind, "Processing DM event");
        return handle_dm(state, event, token).await;
    }
    
    // Extract tags for routing
    let has_h_tag = event.tags.find(TagKind::h()).is_some();
    let a_tags = CommunityHandler::extract_a_tags(event);
    let has_a_tag = !a_tags.is_empty();
    
    // Route based on tags
    if has_h_tag && has_a_tag {
        // Event has both 'h' and 'a' tags - route to both groups and communities
        debug!(event_id = %event_id, "Event has both h and a tags, routing to both");
        let recipients = state.community_handler.route_event(event, relay_url).await;
        return send_notifications_to_recipients(state, event, recipients, token).await;
    } else if has_h_tag {
        // NIP-29 group event
        debug!(event_id = %event_id, "Processing NIP-29 group event");
        return handle_group_message(state, event, token).await;
    } else if has_a_tag {
        // NIP-72 community event
        debug!(event_id = %event_id, "Processing NIP-72 community event");
        return handle_community_event(state, event, relay_url, token).await;
    } else {
        // Regular event - check custom subscriptions
        return handle_custom_subscriptions(state, event, context, token).await;
    }
}

async fn handle_community_event(
    state: &AppState,
    event: &Event,
    relay_url: &str,
    token: CancellationToken,
) -> Result<()> {
    let event_id = event.id;
    let a_tags = CommunityHandler::extract_a_tags(event);
    
    debug!(event_id = %event_id, a_tags = ?a_tags, "Routing community event");
    
    // Get all recipients for this community event
    let recipients = state.community_handler.route_event(event, relay_url).await;
    
    if recipients.is_empty() {
        debug!(event_id = %event_id, "No recipients for community event");
        return Ok(());
    }
    
    info!(event_id = %event_id, recipient_count = recipients.len(), "Sending notifications for community event");
    
    send_notifications_to_recipients(state, event, recipients, token).await
}

async fn send_notifications_to_recipients(
    state: &AppState,
    event: &Event,
    recipients: HashSet<String>,
    token: CancellationToken,
) -> Result<()> {
    let event_id = event.id;
    
    for recipient_pubkey in recipients {
        // Check for cancellation
        if token.is_cancelled() {
            info!("Notification sending cancelled");
            break;
        }
        
        // Parse the pubkey and send notification
        if let Ok(pubkey) = PublicKey::from_str(&recipient_pubkey) {
            if let Err(e) = send_notification_to_user(state, event, &pubkey, token.clone()).await {
                if matches!(e, crate::error::ServiceError::Cancelled) {
                    return Err(e);
                }
                error!(
                    event_id = %event_id,
                    recipient = %recipient_pubkey,
                    error = %e,
                    "Failed to send notification"
                );
            }
        } else {
            error!(
                event_id = %event_id,
                recipient = %recipient_pubkey,
                "Invalid pubkey format"
            );
        }
    }
    
    Ok(())
}

// Placeholder functions for handlers that need to be implemented

async fn handle_registration(state: &AppState, event: &Event) -> Result<()> {
    assert!(event.kind == KIND_REGISTRATION);

    // Validate that content is NIP-44 encrypted
    if let Err(e) = CryptoService::validate_encrypted_content(&event.content) {
        error!(
            event_id = %event.id, pubkey = %event.pubkey, error = %e,
            "Received registration with plaintext token - rejecting"
        );
        return Ok(()); // Don't process plaintext tokens
    }
    
    // Get crypto service from state
    let crypto_service = match &state.crypto_service {
        Some(service) => service,
        None => {
            error!(event_id = %event.id, "No crypto service configured - cannot decrypt tokens");
            return Ok(());
        }
    };
    
    // Decrypt the NIP-44 content
    let token_payload = match crypto_service.decrypt_token_payload(&event.content, &event.pubkey) {
        Ok(payload) => payload,
        Err(e) => {
            error!(
                event_id = %event.id, pubkey = %event.pubkey, error = %e,
                "Failed to decrypt registration token"
            );
            return Ok(()); // Don't fail the whole handler for decryption errors
        }
    };
    
    let fcm_token = token_payload.token.trim();
    if fcm_token.is_empty() {
        warn!(
            event_id = %event.id, pubkey = %event.pubkey,
            "Received registration event with empty token after decryption"
        );
        return Ok(());
    }

    // Extract app tag for namespace isolation
    let app = get_app_tag(event).unwrap_or_else(|| "default".to_string());
    
    match redis_store::add_or_update_token_with_app(&state.redis_pool, &app, &event.pubkey, fcm_token).await {
        Ok(_) => {
            info!(event_id = %event.id, pubkey = %event.pubkey, app = %app, "Registered/Updated encrypted token");
        }
        Err(e) => {
            return Err(e);
        }
    }
    Ok(())
}

async fn handle_deregistration(state: &AppState, event: &Event) -> Result<()> {
    assert!(event.kind == KIND_DEREGISTRATION);

    // Validate that content is NIP-44 encrypted
    if let Err(e) = CryptoService::validate_encrypted_content(&event.content) {
        error!(
            event_id = %event.id, pubkey = %event.pubkey, error = %e,
            "Received deregistration with plaintext token - rejecting"
        );
        return Ok(()); // Don't process plaintext tokens
    }
    
    // Get crypto service from state
    let crypto_service = match &state.crypto_service {
        Some(service) => service,
        None => {
            error!(event_id = %event.id, "No crypto service configured - cannot decrypt tokens");
            return Ok(());
        }
    };
    
    // Decrypt the NIP-44 content
    let token_payload = match crypto_service.decrypt_token_payload(&event.content, &event.pubkey) {
        Ok(payload) => payload,
        Err(e) => {
            error!(
                event_id = %event.id, pubkey = %event.pubkey, error = %e,
                "Failed to decrypt deregistration token"
            );
            return Ok(()); // Don't fail the whole handler for decryption errors
        }
    };
    
    let fcm_token = token_payload.token.trim();
    if fcm_token.is_empty() {
        warn!(
            event_id = %event.id, pubkey = %event.pubkey,
            "Received deregistration event with empty token after decryption"
        );
        return Ok(());
    }

    // Extract app tag for namespace isolation
    let app = get_app_tag(event).unwrap_or_else(|| "default".to_string());
    
    let removed = redis_store::remove_token_with_app(&state.redis_pool, &app, &event.pubkey, fcm_token).await?;
    if removed {
        info!(event_id = %event.id, pubkey = %event.pubkey, app = %app, "Deregistered encrypted token");
    } else {
        debug!(
            event_id = %event.id, pubkey = %event.pubkey, app = %app,
            "Token not found for deregistration"
        );
    }
    
    // Clean up all subscriptions for this user when they deregister
    let pubkey_hex = event.pubkey.to_hex();
    let removed_filters = state.subscription_manager
        .remove_all_user_filters(&app, &pubkey_hex)
        .await;
    
    // Unsubscribe from relay for any filters that are no longer needed
    if !removed_filters.is_empty() {
        let mut user_subs = state.user_subscriptions.write().await;
        for filter_hash in &removed_filters {
            if let Some(subscription_id) = user_subs.remove(filter_hash) {
                state.nostr_client.unsubscribe(&subscription_id).await;
                info!(
                    event_id = %event.id,
                    filter_hash = &filter_hash[..8],
                    subscription_id = %subscription_id,
                    "Unsubscribed from relay during deregistration"
                );
            }
        }
        
        info!(
            event_id = %event.id,
            pubkey = %event.pubkey,
            app = %app,
            removed_count = removed_filters.len(),
            "Cleaned up user subscriptions during deregistration"
        );
    }
    
    // Also clean up any community/group memberships
    state.community_handler.cleanup_user(&pubkey_hex).await;
    
    Ok(())
}

/// Handle DM events (kind 1059) by notifying all recipients in p-tags
pub async fn handle_dm(state: &AppState, event: &Event, token: CancellationToken) -> Result<()> {
    info!(event_id = %event.id, sender = %event.pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()), "Handling DM event");

    if token.is_cancelled() {
        info!(event_id = %event.id, "Cancelled before handling DM.");
        return Err(crate::error::ServiceError::Cancelled);
    }

    // Log all tags for debugging
    info!(event_id = %event.id, "DM event tags count: {}", event.tags.len());
    for (i, tag) in event.tags.iter().enumerate() {
        info!(event_id = %event.id, "Tag {}: kind={:?}, content={:?}", 
            i, tag.kind(), tag.content());
    }
    
    // Extract all p-tags (recipients)
    let recipients: Vec<PublicKey> = event
        .tags
        .iter()
        .filter(|t| t.kind() == TagKind::p())
        .filter_map(|t| t.content())
        .filter_map(|content| PublicKey::from_str(content).ok())
        .collect();

    let recipient_npubs: Vec<String> = recipients
        .iter()
        .map(|pk| pk.to_bech32().unwrap_or_else(|_| "invalid".to_string()))
        .collect();
    
    info!(event_id = %event.id, recipient_count = recipients.len(), recipients = ?recipient_npubs, "Extracted DM recipients from p-tags");

    if recipients.is_empty() {
        info!(event_id = %event.id, "No recipients found in DM p-tags - skipping notification");
        return Ok(());
    }

    for recipient_pubkey in recipients {
        if token.is_cancelled() {
            info!(event_id = %event.id, "Cancelled during DM recipient processing");
            return Err(crate::error::ServiceError::Cancelled);
        }

        // Skip if recipient is the sender (self-DM)
        if recipient_pubkey == event.pubkey {
            trace!(event_id = %event.id, pubkey = %recipient_pubkey, "Skipping DM notification to sender");
            continue;
        }

        info!(event_id = %event.id, recipient = %recipient_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()), "Processing DM for recipient");

        if let Err(e) =
            send_notification_to_user(state, event, &recipient_pubkey, token.clone()).await
        {
            if matches!(e, crate::error::ServiceError::Cancelled) {
                return Err(e);
            }
            error!(event_id = %event.id, recipient = %recipient_pubkey, error = %e, "Failed to send DM notification");
        }
    }

    info!(event_id = %event.id, "Finished handling DM");
    Ok(())
}

pub async fn handle_subscription_upsert(
    state: &AppState,
    event: &Event,
    token: CancellationToken,
) -> Result<()> {

    if token.is_cancelled() {
        info!(event_id = %event.id, "Cancelled before handling subscription upsert.");
        return Err(crate::error::ServiceError::Cancelled);
    }

    // Validate that content is NIP-44 encrypted
    if let Err(e) = CryptoService::validate_encrypted_content(&event.content) {
        error!(
            event_id = %event.id, pubkey = %event.pubkey, error = %e,
            "Received subscription with plaintext content - rejecting"
        );
        return Ok(()); // Don't process plaintext filters
    }
    
    // Get crypto service from state
    let crypto_service = match &state.crypto_service {
        Some(service) => service,
        None => {
            error!(event_id = %event.id, "No crypto service configured - cannot decrypt filters");
            return Ok(());
        }
    };
    
    // Decrypt the NIP-44 content
    let filter_payload = match crypto_service.decrypt_filter_upsert_payload(&event.content, &event.pubkey) {
        Ok(payload) => payload,
        Err(e) => {
            error!(
                event_id = %event.id, pubkey = %event.pubkey, error = %e,
                "Failed to decrypt subscription filter"
            );
            return Ok(()); // Don't fail the whole handler for decryption errors
        }
    };
    
    // Parse the filter from the decrypted payload
    let filter: Filter = match serde_json::from_value(filter_payload.filter.clone()) {
        Ok(f) => f,
        Err(e) => {
            warn!(event_id = %event.id, error = %e, "Invalid filter in decrypted payload");
            return Ok(());
        }
    };
    
    // Extract app tag for namespace isolation
    let app = get_app_tag(event).unwrap_or_else(|| "default".to_string());
    
    // Check if this app is supported
    if !state.supported_apps.contains(&app) {
        warn!(
            event_id = %event.id, 
            pubkey = %event.pubkey,
            app = %app,
            "Subscription request for unsupported app"
        );
        return Ok(());
    }
    
    // Check if the filter kinds are allowed for this app
    if let Some(app_config) = state.settings.apps.iter().find(|a| a.name == app) {
        // Extract kinds from the filter
        let filter_json = serde_json::to_value(&filter).map_err(|e| {
            crate::error::ServiceError::Internal(format!("Failed to serialize filter: {}", e))
        })?;
        
        if let Some(kinds) = filter_json.get("kinds").and_then(|k| k.as_array()) {
            for kind_val in kinds {
                if let Some(kind_num) = kind_val.as_u64() {
                    if !app_config.allowed_subscription_kinds.contains(&kind_num) {
                        warn!(
                            event_id = %event.id,
                            pubkey = %event.pubkey,
                            app = %app,
                            kind = kind_num,
                            "Filter contains disallowed kind for app"
                        );
                        return Ok(());
                    }
                }
            }
        }
    }

    // Add filter to subscription manager and check if it's new
    let pubkey_hex = event.pubkey.to_hex();
    let (filter_hash, is_new_filter) = state.subscription_manager
        .add_user_filter(app.clone(), pubkey_hex.clone(), filter.clone())
        .await;
    
    // Check if filter contains DM kinds - if so, skip relay subscription (DMs use global subscription)
    let filter_json = serde_json::to_value(&filter).map_err(|e| {
        crate::error::ServiceError::Internal(format!("Failed to serialize filter for DM check: {}", e))
    })?;
    
    let has_dm_kinds = if let Some(kinds) = filter_json.get("kinds").and_then(|k| k.as_array()) {
        kinds.iter().any(|kind_val| {
            if let Some(kind_num) = kind_val.as_u64() {
                state.settings.service.dm_kinds.contains(&kind_num)
            } else {
                false
            }
        })
    } else {
        false
    };
    
    // If this is a new filter (ref_count went from 0->1), create relay subscription
    // Skip relay subscription for DM kinds (they use global subscription)
    if is_new_filter && !has_dm_kinds {
        match state.nostr_client.subscribe(filter.clone(), None).await {
            Ok(output) => {
                let subscription_id = output.val;
                let mut user_subs = state.user_subscriptions.write().await;
                user_subs.insert(filter_hash.clone(), subscription_id.clone());
                info!(
                    event_id = %event.id,
                    filter_hash = &filter_hash[..8],
                    subscription_id = %subscription_id,
                    "Created new relay subscription for filter"
                );
            }
            Err(e) => {
                error!(
                    event_id = %event.id,
                    filter_hash = &filter_hash[..8],
                    error = %e,
                    "Failed to create relay subscription"
                );
                // Roll back the subscription manager state
                state.subscription_manager.remove_user_filter(&app, &pubkey_hex, &filter_hash).await;
                return Err(crate::error::ServiceError::Internal(
                    format!("Failed to create relay subscription: {}", e)
                ));
            }
        }
    } else if is_new_filter && has_dm_kinds {
        info!(
            event_id = %event.id,
            filter_hash = &filter_hash[..8],
            "Skipped relay subscription for DM kinds (using global DM subscription)"
        );
    }

    // Convert filter to JSON Value for storage
    let filter_value = serde_json::to_value(&filter).map_err(|e| {
        crate::error::ServiceError::Internal(format!("Failed to serialize filter: {}", e))
    })?;

    // Store the subscription in Redis for persistence
    let subscription_timestamp = event.created_at.as_u64();
    
    redis_store::add_subscription_by_filter(
        &state.redis_pool,
        &app,
        &event.pubkey, 
        &filter_value,
        subscription_timestamp
    ).await?;

    info!(
        event_id = %event.id,
        app = %app,
        filter_hash = &filter_hash[..8],
        "Successfully stored subscription"
    );
    Ok(())
}


pub async fn handle_subscription_delete(
    state: &AppState,
    event: &Event,
    token: CancellationToken,
) -> Result<()> {
    debug!(event_id = %event.id, "Handling subscription delete");

    if token.is_cancelled() {
        info!(event_id = %event.id, "Cancelled before handling subscription delete.");
        return Err(crate::error::ServiceError::Cancelled);
    }

    // Validate that content is NIP-44 encrypted
    if let Err(e) = CryptoService::validate_encrypted_content(&event.content) {
        error!(
            event_id = %event.id, pubkey = %event.pubkey, error = %e,
            "Received subscription delete with plaintext content - rejecting"
        );
        return Ok(()); // Don't process plaintext
    }
    
    // Get crypto service from state
    let crypto_service = match &state.crypto_service {
        Some(service) => service,
        None => {
            error!(event_id = %event.id, "No crypto service configured - cannot decrypt");
            return Ok(());
        }
    };
    
    // Decrypt the NIP-44 content
    let delete_payload = match crypto_service.decrypt_filter_delete_payload(&event.content, &event.pubkey) {
        Ok(payload) => payload,
        Err(e) => {
            error!(
                event_id = %event.id, pubkey = %event.pubkey, error = %e,
                "Failed to decrypt subscription delete payload"
            );
            return Ok(()); // Don't fail the whole handler for decryption errors
        }
    };

    // Extract app tag for namespace isolation
    let app = get_app_tag(event).unwrap_or_else(|| "default".to_string());
    
    // Parse the filter from the delete payload
    let filter: Filter = match serde_json::from_value(delete_payload.filter.clone()) {
        Ok(f) => f,
        Err(e) => {
            warn!(event_id = %event.id, error = %e, "Invalid filter in delete payload");
            return Ok(());
        }
    };
    
    // Compute the filter hash to find the subscription
    let filter_hash = SubscriptionManager::compute_filter_hash(&filter);
    let pubkey_hex = event.pubkey.to_hex();
    
    // Remove from subscription manager and check if it was the last reference
    let was_removed = state.subscription_manager
        .remove_user_filter(&app, &pubkey_hex, &filter_hash)
        .await;
    
    // If the filter was completely removed (ref_count reached 0), unsubscribe from relay
    if was_removed {
        let mut user_subs = state.user_subscriptions.write().await;
        if let Some(subscription_id) = user_subs.remove(&filter_hash) {
            state.nostr_client.unsubscribe(&subscription_id).await;
            info!(
                event_id = %event.id,
                filter_hash = &filter_hash[..8],
                subscription_id = %subscription_id,
                "Unsubscribed from relay (no more users for this filter)"
            );
        }
    }

    // Remove the subscription from Redis storage
    let removed = redis_store::remove_subscription_by_filter(
        &state.redis_pool, 
        &app, 
        &event.pubkey, 
        &delete_payload.filter
    ).await?;
    
    if removed {
        info!(
            event_id = %event.id, 
            pubkey = %event.pubkey, 
            app = %app, 
            filter_hash = &filter_hash[..8],
            was_last_ref = was_removed,
            "Removed subscription by filter hash"
        );
    } else {
        warn!(
            event_id = %event.id, 
            pubkey = %event.pubkey, 
            filter_hash = &filter_hash[..8],
            "Subscription with specified filter not found in Redis"
        );
    }
    Ok(())
}

/// Handle group-scoped events (events with 'h' tag) by checking mentions, broadcasts, and custom subscriptions with group membership validation.
async fn handle_group_message(
    state: &AppState,
    event: &Event,
    token: CancellationToken,
) -> Result<()> {
    debug!(event_id = %event.id, kind = %event.kind, "Handling group-scoped event");

    if token.is_cancelled() {
        info!(event_id = %event.id, "Cancelled before handling group message.");
        return Err(crate::error::ServiceError::Cancelled);
    }

    // Extract the group ID from the 'h' tag (we know it exists because the dispatcher checked)
    let group_id = event.tags.find(TagKind::h())
        .and_then(|tag| tag.content())
        .ok_or_else(|| {
            error!(event_id = %event.id, "Expected 'h' tag but not found");
            crate::error::ServiceError::Internal("Missing expected 'h' tag".to_string())
        })?;
    
    debug!(event_id = %event.id, %group_id, "Processing group-scoped event");

    // Check if this is a broadcast message (has a 'broadcast' tag)
    let is_broadcast = event.tags.find(TagKind::custom("broadcast")).is_some();

    if is_broadcast {
        debug!(event_id = %event.id, %group_id, "Broadcast tag detected, will notify all group members");
        return handle_broadcast_message(state, event, group_id, token).await;
    }

    // Handle mentions
    let mentioned_pubkeys = extract_mentioned_pubkeys(event);
    debug!(event_id = %event.id, mentions = mentioned_pubkeys.len(), "Extracted mentioned pubkeys");

    for target_pubkey in mentioned_pubkeys {
        if token.is_cancelled() {
            info!(event_id = %event.id, "Cancelled during group message pubkey processing loop.");
            return Err(crate::error::ServiceError::Cancelled);
        }

        trace!(event_id = %event.id, target_pubkey = %target_pubkey, %group_id, "Processing mention for group");

        if target_pubkey == event.pubkey {
            info!(
                event_id = %event.id, 
                target_npub = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()),
                sender_npub = %event.pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()),
                "Skipping notification to self in group message"
            );
            continue;
        }

        // Check if the mentioned user is a member of the group
        match state
            .nip29_client
            .is_group_member(group_id, &target_pubkey)
            .await
        {
            Ok(true) => {
                trace!(event_id = %event.id, target_pubkey = %target_pubkey, %group_id, "Target user is a group member. Proceeding with notification.");
                // User is a member, continue to send notification
            }
            Ok(false) => {
                debug!(event_id = %event.id, target_pubkey = %target_pubkey, %group_id, "Target user is not a member of the group. Skipping notification.");
                continue; // Skip this user, move to the next mentioned pubkey
            }
            Err(e) => {
                error!(event_id = %event.id, target_pubkey = %target_pubkey, %group_id, error = %e, "Failed to check group membership. Skipping notification for this user.");
                continue; // Skip this user due to error, move to the next
            }
        }
        
        info!(
            event_id = %event.id,
            target_npub = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()),
            sender_npub = %event.pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()),
            group_id = %group_id,
            "Sending notification for group mention"
        );

        if let Err(e) = send_notification_to_user(state, event, &target_pubkey, token.clone()).await
        {
            if matches!(e, crate::error::ServiceError::Cancelled) {
                return Err(e);
            }
            error!(event_id = %event.id, target_pubkey = %target_pubkey, error = %e, "Failed to send notification to user");
        }
    }

    // Check custom subscriptions for this group with membership validation
    check_subscriptions_for_matching_users(state, event, Some(group_id), token).await?;

    debug!(event_id = %event.id, "Finished handling group message/reply");
    Ok(())
}

/// Handle a broadcast message by sending notifications to all group members.
#[instrument(skip_all, fields(%group_id))]
async fn handle_broadcast_message(
    state: &AppState,
    event: &Event,
    group_id: &str,
    token: CancellationToken,
) -> Result<()> {
    info!(event_id = %event.id, %group_id, "Processing broadcast message");

    let kind_allowed = BROADCASTABLE_EVENT_KINDS.contains(&event.kind);
    if !kind_allowed {
        warn!(
            event_id = %event.id,
            kind = %event.kind,
            "Event kind is not allowed for broadcast. Skipping.",
        );
        return Ok(());
    }
    debug!(event_id = %event.id, kind = %event.kind, "Event kind is allowed for broadcast");

    let admin_check_result = state
        .nip29_client
        .is_group_admin(group_id, &event.pubkey)
        .await;

    match admin_check_result {
        Ok(true) => {
            debug!(event_id = %event.id, pubkey = %event.pubkey, "Sender IS an admin. Proceeding with broadcast.");
        }
        Ok(false) => {
            warn!(event_id = %event.id, pubkey = %event.pubkey, "Sender is NOT an admin. Rejecting broadcast attempt.");
            return Ok(()); // Not an error, just reject the unauthorized broadcast
        }
        Err(e) => {
            error!(event_id = %event.id, pubkey = %event.pubkey, error = %e, "Failed to check admin status for broadcast. Skipping.");
            return Err(e);
        }
    }

    let members_result = state.nip29_client.get_group_members(group_id).await;

    let members = match members_result {
        Ok(members) => {
            info!(event_id = %event.id, %group_id, count = members.len(), "Retrieved group members for broadcast");
            members
        }
        Err(e) => {
            error!(event_id = %event.id, %group_id, error = %e, "Failed to get group members for broadcast");
            return Err(e);
        }
    };

    info!(
        event_id = %event.id, %group_id,
        "Proceeding to send notifications to {} members (excluding sender)",
        members.len().saturating_sub(1)
    );

    let sender_pubkey = event.pubkey;
    let mut success_count = 0;
    let mut error_count = 0;
    let member_count = members.len();

    let members_vec: Vec<_> = members.into_iter().collect();
    for member_pubkey in members_vec {
        if token.is_cancelled() {
            info!(event_id = %event.id, %group_id, "Cancelled during broadcast processing");
            return Err(crate::error::ServiceError::Cancelled);
        }

        if member_pubkey == sender_pubkey {
            trace!(event_id = %event.id, target_pubkey = %member_pubkey, "Skipping notification to sender");
            continue;
        }

        match send_notification_to_user(state, event, &member_pubkey, token.clone()).await {
            Ok(_) => {
                success_count += 1;
            }
            Err(e) => {
                if matches!(e, crate::error::ServiceError::Cancelled) {
                    return Err(e);
                }
                error!(event_id = %event.id, target_pubkey = %member_pubkey, error = %e, "Failed to send broadcast notification to user");
                error_count += 1;
            }
        }
    }

    info!(
        event_id = %event.id, %group_id,
        total = member_count - 1, // Subtract 1 to account for sender
        success = success_count,
        errors = error_count,
        "Broadcast notification completed"
    );

    Ok(())
}

/// Send a notification to a specific user
#[instrument(skip_all, fields(target_pubkey = %target_pubkey.to_string()))]
async fn send_notification_to_user(
    state: &AppState,
    event: &Event,
    target_pubkey: &PublicKey,
    token: CancellationToken,
) -> Result<()> {
    let event_id = event.id;

    // For DMs and broadcasts, get tokens grouped by app from ALL app namespaces
    // This ensures users receive notifications regardless of which app they registered with
    let tokens_by_app = tokio::select! {
        biased;
        _ = token.cancelled() => {
            info!(event_id = %event_id, target_pubkey = %target_pubkey, "Cancelled while fetching tokens.");
            return Err(crate::error::ServiceError::Cancelled);
        }
        res = redis_store::get_tokens_by_app_for_pubkey(&state.redis_pool, target_pubkey) => {
            res?
        }
    };
    if tokens_by_app.is_empty() {
        info!(event_id = %event_id, target_pubkey = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()), "No FCM tokens registered for recipient - skipping notification");
        return Ok(());
    }
    
    // For DM events, filter tokens to only include apps where user has subscribed to DM kinds
    let filtered_tokens_by_app = if is_dm_event(event) {
        let mut filtered = std::collections::HashMap::new();
        let target_pubkey_hex = target_pubkey.to_hex();
        
        for (app, tokens) in tokens_by_app {
            // Check if user has any filters with DM kinds for this app
            let user_filters = state.subscription_manager.get_user_filters(&app, &target_pubkey_hex).await;
            let mut has_dm_subscription = false;
            
            for filter_hash in user_filters {
                if let Some(filter) = state.subscription_manager.get_filter_by_hash(&filter_hash).await {
                    // Check if any kind in the filter is a DM kind
                    let filter_json = serde_json::to_value(&filter).unwrap_or_default();
                    if let Some(kinds) = filter_json.get("kinds").and_then(|k| k.as_array()) {
                        has_dm_subscription = kinds.iter().any(|kind_val| {
                            if let Some(kind_num) = kind_val.as_u64() {
                                state.settings.service.dm_kinds.contains(&kind_num)
                            } else {
                                false
                            }
                        });
                        if has_dm_subscription {
                            break;
                        }
                    }
                }
            }
            
            if has_dm_subscription {
                info!(event_id = %event_id, target_pubkey = %target_pubkey_hex, app = %app, "User has DM subscription - including tokens");
                filtered.insert(app, tokens);
            } else {
                info!(event_id = %event_id, target_pubkey = %target_pubkey_hex, app = %app, "User has NO DM subscription - skipping tokens");
            }
        }
        filtered
    } else {
        tokens_by_app
    };
    
    if filtered_tokens_by_app.is_empty() {
        if is_dm_event(event) {
            info!(event_id = %event_id, target_pubkey = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()), "No DM subscriptions found for recipient - skipping notification");
        } else {
            info!(event_id = %event_id, target_pubkey = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()), "No tokens after filtering - skipping notification");
        }
        return Ok(());
    }

    let total_tokens: usize = filtered_tokens_by_app.values().map(|v| v.len()).sum();
    info!(event_id = %event_id, target_pubkey = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()), token_count = total_tokens, app_count = filtered_tokens_by_app.len(), "Found FCM tokens for recipient");

    info!(event_id = %event_id, target_pubkey = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()), "Creating FCM payload for notification");
    let payload = create_fcm_payload(event, target_pubkey, state).await?;

    // Send notifications using the appropriate FCM client for each app
    let mut all_results = std::collections::HashMap::new();
    
    for (app, tokens) in filtered_tokens_by_app {
        // Get the FCM client for this app
        let fcm_client = match state.fcm_clients.get(&app) {
            Some(client) => client,
            None => {
                warn!(event_id = %event_id, app = %app, "No FCM client configured for app - skipping tokens");
                continue;
            }
        };
        
        info!(event_id = %event_id, target_pubkey = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()), app = %app, token_count = tokens.len(), "Attempting to send FCM notification for app");
        
        let results = tokio::select! {
            biased;
            _ = token.cancelled() => {
                info!(event_id = %event_id, target_pubkey = %target_pubkey, "Cancelled during FCM send batch.");
                return Err(crate::error::ServiceError::Cancelled);
            }
            send_result = fcm_client.send_batch(&tokens, payload.clone()) => {
                send_result
            }
        };
        
        // Merge results
        all_results.extend(results);
    }
    
    let results = all_results;
    info!(event_id = %event_id, target_pubkey = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()), results_count = results.len(), "FCM send completed");

    trace!(event_id = %event_id, target_pubkey = %target_pubkey, "Handling FCM results");
    let mut tokens_to_remove = Vec::new();
    let mut success_count = 0;
    for (fcm_token, result) in results {
        if token.is_cancelled() {
            info!(event_id = %event_id, target_pubkey = %target_pubkey, "Cancelled while processing FCM results.");
            return Err(crate::error::ServiceError::Cancelled);
        }

        let truncated_token = &fcm_token[..8.min(fcm_token.len())];

        match result {
            Ok(_) => {
                success_count += 1;
                trace!(target_pubkey = %target_pubkey, token_prefix = truncated_token, "Successfully sent notification");
            }
            Err(fcm_sender::FcmError::TokenNotRegistered) => {
                warn!(target_pubkey = %target_pubkey, token_prefix = truncated_token, "Token invalid/unregistered, marking for removal.");
                tokens_to_remove.push(fcm_token);
            }
            Err(e) => {
                error!(
                    target_pubkey = %target_pubkey, token_prefix = truncated_token, error = %e, error_debug = ?e,
                    "FCM send failed for token"
                );
            }
        }
    }
    info!(event_id = %event_id, target_pubkey = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()), success_count, failed_count = tokens_to_remove.len(), "FCM notification send summary");

    if !tokens_to_remove.is_empty() {
        debug!(event_id = %event_id, target_pubkey = %target_pubkey, count = tokens_to_remove.len(), "Removing invalid tokens globally");
        for fcm_token_to_remove in tokens_to_remove {
            if token.is_cancelled() {
                info!(event_id = %event_id, target_pubkey = %target_pubkey, "Cancelled while removing invalid tokens.");
                return Err(crate::error::ServiceError::Cancelled);
            }
            let truncated_token = &fcm_token_to_remove[..8.min(fcm_token_to_remove.len())];
            if let Err(e) = redis_store::remove_token_globally(
                &state.redis_pool,
                target_pubkey,
                &fcm_token_to_remove,
            )
            .await
            {
                error!(
                    target_pubkey = %target_pubkey, token_prefix = truncated_token, error = %e,
                    "Failed to remove invalid token globally"
                );
            } else {
                info!(target_pubkey = %target_pubkey, token_prefix = truncated_token, "Removed invalid token globally");
            }
        }
    } else {
        trace!(event_id = %event_id, target_pubkey = %target_pubkey, "No invalid tokens to remove");
    }
    trace!(event_id = %event_id, target_pubkey = %target_pubkey, "Finished sending notification");

    Ok(())
}

/// Send notification to user for a specific app only.
/// This is used for custom subscriptions that should only trigger notifications
/// to the app where the subscription was created.
#[instrument(skip_all, fields(target_pubkey = %target_pubkey.to_string(), app = %app))]
async fn send_notification_to_user_for_app(
    state: &AppState,
    event: &Event,
    target_pubkey: &PublicKey,
    app: &str,
    token: CancellationToken,
) -> Result<()> {
    let event_id = event.id;

    // Get tokens only for the specific app
    let tokens = tokio::select! {
        biased;
        _ = token.cancelled() => {
            info!(event_id = %event_id, target_pubkey = %target_pubkey, app = %app, "Cancelled while fetching tokens.");
            return Err(crate::error::ServiceError::Cancelled);
        }
        res = redis_store::get_tokens_for_pubkey_with_app(&state.redis_pool, app, target_pubkey) => {
            res?
        }
    };
    
    if tokens.is_empty() {
        info!(
            event_id = %event_id, 
            target_pubkey = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()),
            app = %app,
            "No FCM tokens registered for recipient in app - skipping notification"
        );
        return Ok(());
    }
    
    info!(
        event_id = %event_id, 
        target_pubkey = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()),
        app = %app,
        token_count = tokens.len(),
        "Found FCM tokens for recipient in app"
    );

    // Get the FCM client for this app
    let fcm_client = match state.fcm_clients.get(app) {
        Some(client) => client,
        None => {
            warn!(event_id = %event_id, app = %app, "No FCM client configured for app - cannot send notification");
            return Ok(());
        }
    };

    info!(event_id = %event_id, target_pubkey = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()), "Creating FCM payload for notification");
    let payload = create_fcm_payload(event, target_pubkey, state).await?;

    info!(
        event_id = %event_id,
        target_pubkey = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()),
        app = %app,
        token_count = tokens.len(),
        "Attempting to send FCM notification for app"
    );
    
    let results = tokio::select! {
        biased;
        _ = token.cancelled() => {
            info!(event_id = %event_id, target_pubkey = %target_pubkey, app = %app, "Cancelled during FCM send batch.");
            return Err(crate::error::ServiceError::Cancelled);
        }
        send_result = fcm_client.send_batch(&tokens, payload) => {
            send_result
        }
    };
    
    info!(
        event_id = %event_id,
        target_pubkey = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()),
        app = %app,
        results_count = results.len(),
        "FCM send completed"
    );

    trace!(event_id = %event_id, target_pubkey = %target_pubkey, app = %app, "Handling FCM results");
    let mut tokens_to_remove = Vec::new();
    let mut success_count = 0;
    
    for (fcm_token, result) in results {
        if token.is_cancelled() {
            info!(event_id = %event_id, target_pubkey = %target_pubkey, app = %app, "Cancelled while processing FCM results.");
            return Err(crate::error::ServiceError::Cancelled);
        }

        let truncated_token = &fcm_token[..8.min(fcm_token.len())];

        match result {
            Ok(_) => {
                success_count += 1;
                trace!(target_pubkey = %target_pubkey, app = %app, token_prefix = truncated_token, "Successfully sent notification");
            }
            Err(fcm_sender::FcmError::TokenNotRegistered) => {
                warn!(target_pubkey = %target_pubkey, app = %app, token_prefix = truncated_token, "Token invalid/unregistered, marking for removal.");
                tokens_to_remove.push(fcm_token);
            }
            Err(e) => {
                error!(
                    target_pubkey = %target_pubkey, app = %app, token_prefix = truncated_token, error = %e, error_debug = ?e,
                    "FCM send failed for token"
                );
            }
        }
    }
    
    info!(
        event_id = %event_id,
        target_pubkey = %target_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()),
        app = %app,
        success_count,
        failed_count = tokens_to_remove.len(),
        "FCM notification send summary"
    );

    if !tokens_to_remove.is_empty() {
        debug!(event_id = %event_id, target_pubkey = %target_pubkey, app = %app, count = tokens_to_remove.len(), "Removing invalid tokens from app");
        for fcm_token_to_remove in tokens_to_remove {
            if token.is_cancelled() {
                info!(event_id = %event_id, target_pubkey = %target_pubkey, app = %app, "Cancelled while removing invalid tokens.");
                return Err(crate::error::ServiceError::Cancelled);
            }
            let truncated_token = &fcm_token_to_remove[..8.min(fcm_token_to_remove.len())];
            if let Err(e) = redis_store::remove_token_with_app(
                &state.redis_pool,
                app,
                target_pubkey,
                &fcm_token_to_remove,
            )
            .await
            {
                error!(
                    target_pubkey = %target_pubkey, app = %app, token_prefix = truncated_token, error = %e,
                    "Failed to remove invalid token from app"
                );
            } else {
                info!(target_pubkey = %target_pubkey, app = %app, token_prefix = truncated_token, "Removed invalid token from app");
            }
        }
    } else {
        trace!(event_id = %event_id, target_pubkey = %target_pubkey, app = %app, "No invalid tokens to remove");
    }
    
    trace!(event_id = %event_id, target_pubkey = %target_pubkey, app = %app, "Finished sending notification to app");

    Ok(())
}


/// Check subscriptions for users and send notifications if filters match.
///
/// If group_id is provided, validates group membership before sending notifications.
/// This is used for NIP-29 group events where only members should receive notifications.
async fn check_subscriptions_for_matching_users(
    state: &AppState,
    event: &Event,
    group_id: Option<&str>,
    token: CancellationToken,
) -> Result<()> {
    let users_with_subscriptions =
        redis_store::get_all_users_with_subscriptions(&state.redis_pool).await?;

    for user_pubkey in users_with_subscriptions {
        if token.is_cancelled() {
            return Err(crate::error::ServiceError::Cancelled);
        }

        // Skip if this is the sender
        if user_pubkey == event.pubkey {
            continue;
        }

        // Get tokens grouped by app
        let tokens_by_app = redis_store::get_tokens_by_app_for_pubkey(&state.redis_pool, &user_pubkey).await?;
        if tokens_by_app.is_empty() {
            continue;
        }

        // Get user's subscriptions
        let subscriptions_with_timestamps = redis_store::get_all_subscriptions_by_hash(&state.redis_pool, &user_pubkey).await?;

        let event_timestamp = event.created_at.as_u64();
        let user_pubkey_hex = user_pubkey.to_hex();

        for (app, filter_value, subscription_timestamp) in subscriptions_with_timestamps {
            // Lazy cleanup
            if !tokens_by_app.contains_key(&app) {
                let _ = redis_store::remove_subscription_by_filter(
                    &state.redis_pool,
                    &app,
                    &user_pubkey,
                    &filter_value
                ).await;

                if let Ok(filter) = serde_json::from_value::<Filter>(filter_value.clone()) {
                    let filter_hash = SubscriptionManager::compute_filter_hash(&filter);
                    let was_removed = state.subscription_manager.remove_user_filter(&app, &user_pubkey_hex, &filter_hash).await;

                    if was_removed {
                        let mut user_subs = state.user_subscriptions.write().await;
                        if let Some(subscription_id) = user_subs.remove(&filter_hash) {
                            state.nostr_client.unsubscribe(&subscription_id).await;
                        }
                    }
                }
                continue;
            }

            // Check timestamp
            if event_timestamp <= subscription_timestamp {
                continue;
            }

            // Parse filter (clone filter_value since we might need it later for cleanup)
            let filter: Filter = match serde_json::from_value(filter_value.clone()) {
                Ok(f) => f,
                Err(_) => continue,
            };

            // Check if filter matches
            if filter.match_event(event) {
                // If group_id provided, validate membership
                if let Some(gid) = group_id {
                    match state.nip29_client.is_group_member(gid, &user_pubkey).await {
                        Ok(true) => {
                            debug!(event_id = %event.id, user = %user_pubkey_hex, group_id = %gid, "User is group member with matching subscription");
                        }
                        Ok(false) => {
                            debug!(event_id = %event.id, user = %user_pubkey_hex, group_id = %gid, "User not a group member - checking if subscription is specific to this group");

                            // Check if filter is specific to THIS group only
                            // Safe to remove only if filter targets exactly one h-tag matching this group
                            let h_tag_key = SingleLetterTag::lowercase(Alphabet::H);
                            let is_specific_to_this_group = if let Some(h_values) = filter.generic_tags.get(&h_tag_key) {
                                h_values.len() == 1 && h_values.contains(gid)
                            } else {
                                false
                            };

                            if is_specific_to_this_group {
                                info!(event_id = %event.id, user = %user_pubkey_hex, group_id = %gid, "Removing orphaned group subscription (user not a member)");

                                // Remove subscription from Redis
                                let _ = redis_store::remove_subscription_by_filter(
                                    &state.redis_pool,
                                    &app,
                                    &user_pubkey,
                                    &filter_value
                                ).await;

                                // Remove from subscription manager
                                let filter_hash = SubscriptionManager::compute_filter_hash(&filter);
                                let was_removed = state.subscription_manager.remove_user_filter(&app, &user_pubkey_hex, &filter_hash).await;

                                // Unsubscribe from relay if no one else needs this filter
                                if was_removed {
                                    let mut user_subs = state.user_subscriptions.write().await;
                                    if let Some(subscription_id) = user_subs.remove(&filter_hash) {
                                        state.nostr_client.unsubscribe(&subscription_id).await;
                                        info!(event_id = %event.id, filter_hash = &filter_hash[..8], "Unsubscribed from relay after removing orphaned group subscription");
                                    }
                                }
                            } else {
                                debug!(event_id = %event.id, user = %user_pubkey_hex, "Filter targets multiple groups or no specific h-tag - keeping subscription");
                            }

                            break;
                        }
                        Err(e) => {
                            warn!(event_id = %event.id, user = %user_pubkey_hex, error = %e, "Failed to check group membership");
                            break;
                        }
                    }
                }

                // Send notification
                if let Err(e) = send_notification_to_user_for_app(state, event, &user_pubkey, &app, token.clone()).await {
                    if matches!(e, crate::error::ServiceError::Cancelled) {
                        return Err(e);
                    }
                }
                break; // One match per user is enough
            }
        }
    }

    Ok(())
}

pub async fn handle_custom_subscriptions(
    state: &AppState,
    event: &Event,
    context: EventContext,
    token: CancellationToken,
) -> Result<()> {
    info!(event_id = %event.id, kind = %event.kind, sender = %event.pubkey.to_hex(), "Handling event with custom subscriptions");

    if token.is_cancelled() {
        info!(event_id = %event.id, "Cancelled before handling custom subscriptions.");
        return Err(crate::error::ServiceError::Cancelled);
    }

    // Check custom subscriptions (without group membership validation)
    check_subscriptions_for_matching_users(state, event, None, token.clone()).await?;

    // Track which users we've already processed to avoid duplicates
    let mut processed_users = std::collections::HashSet::new();

    // Build set of users who already got notifications via subscriptions
    let users_with_subscriptions = redis_store::get_all_users_with_subscriptions(&state.redis_pool).await?;
    for user_pubkey in users_with_subscriptions {
        let tokens_by_app = redis_store::get_tokens_by_app_for_pubkey(&state.redis_pool, &user_pubkey).await?;
        if !tokens_by_app.is_empty() {
            let subscriptions = redis_store::get_all_subscriptions_by_hash(&state.redis_pool, &user_pubkey).await?;
            for (_, filter_value, subscription_timestamp) in subscriptions {
                if event.created_at.as_u64() > subscription_timestamp {
                    if let Ok(filter) = serde_json::from_value::<Filter>(filter_value) {
                        if filter.match_event(event) {
                            processed_users.insert(user_pubkey);
                            break;
                        }
                    }
                }
            }
        }
    }

    // Second, check for mentions - process any mentioned users who have tokens
    // This ensures users without subscriptions still get notified when mentioned
    // Skip this for kind 9/10 events as they're already handled by handle_group_message
    if event.kind == Kind::Custom(9) || event.kind == Kind::Custom(10) {
        debug!(event_id = %event.id, "Skipping mention processing for kind 9/10 (handled by handle_group_message)");
        return Ok(());
    }
    
    // Skip mention processing for historical events - they should not trigger mention notifications
    if matches!(context, EventContext::Historical) {
        debug!(event_id = %event.id, "Skipping mention processing for historical event");
        return Ok(());
    }
    
    let mentioned_pubkeys = extract_mentioned_pubkeys(event);
    debug!(event_id = %event.id, mentions = mentioned_pubkeys.len(), "Checking mentioned users");

    for mentioned_pubkey in mentioned_pubkeys {
        if token.is_cancelled() {
            info!(event_id = %event.id, "Cancelled during mention processing");
            return Err(crate::error::ServiceError::Cancelled);
        }

        // Skip if this is the sender (don't notify user of their own mentions)
        if mentioned_pubkey == event.pubkey {
            info!(
                event_id = %event.id,
                mentioned_npub = %mentioned_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()),
                sender_npub = %event.pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()),
                "Skipping notification for self-mention in custom subscriptions"
            );
            continue;
        }

        // Skip if we already processed this user via subscriptions
        if processed_users.contains(&mentioned_pubkey) {
            continue;
        }

        // Check if user has tokens (meaning they've registered for push) - check all namespaces
        let tokens =
            redis_store::get_all_tokens_for_pubkey(&state.redis_pool, &mentioned_pubkey).await?;
        if tokens.is_empty() {
            trace!(event_id = %event.id, user = %mentioned_pubkey, "Mentioned user has no tokens, skipping");
            continue;
        }

        // Send notification for the mention
        trace!(event_id = %event.id, user = %mentioned_pubkey, "User mentioned and has tokens, sending notification");
        if let Err(e) =
            send_notification_to_user(state, event, &mentioned_pubkey, token.clone()).await
        {
            if matches!(e, crate::error::ServiceError::Cancelled) {
                return Err(e);
            }
            error!(event_id = %event.id, user = %mentioned_pubkey, error = %e, "Failed to send notification for mention");
        }
    }

    debug!(event_id = %event.id, "Finished handling custom subscriptions and mentions");
    Ok(())
}

/// Check if an event mentions a specific user
#[allow(dead_code)]
fn event_mentions_user(event: &Event, user_pubkey: &PublicKey) -> bool {
    event.tags
        .iter()
        .filter(|t| t.kind() == TagKind::p())
        .filter_map(|t| t.content())
        .filter_map(|content| PublicKey::from_str(content).ok())
        .any(|mentioned| mentioned == *user_pubkey)
}

// Extracts pubkeys mentioned in any tag starting with "p".
// Delegates to the helper in the nip29 module.

fn extract_mentioned_pubkeys(event: &Event) -> Vec<nostr_sdk::PublicKey> {
    nip29::extract_pubkeys_from_p_tags(&event.tags).collect()
}

/// Check if an event is too old based on the replay horizon
pub fn is_event_too_old(event: &Event) -> bool {
    use std::time::Duration;

    let horizon = Timestamp::now() - Duration::from_secs(REPLAY_HORIZON_DAYS * 24 * 60 * 60);
    event.created_at < horizon
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ProfileCache {
    display_name: String,
    fetched_at: i64,
}

impl ProfileCache {
    fn is_valid(&self, ttl_secs: u64) -> bool {
        let now = chrono::Utc::now().timestamp();
        (now - self.fetched_at) < ttl_secs as i64
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct GroupMetadata {
    name: String,
    uuid: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct GroupMetadataCache {
    name: String,
    uuid: String,
    fetched_at: i64,
}

impl GroupMetadataCache {
    fn is_valid(&self, ttl_secs: u64) -> bool {
        let now = chrono::Utc::now().timestamp();
        (now - self.fetched_at) < ttl_secs as i64
    }
}

async fn fetch_sender_name(
    pubkey: &PublicKey,
    pool: &redis_store::RedisPool,
    client: &Arc<Client>,
    config: &crate::config::NotificationSettings,
) -> String {
    let cache_key = format!("profile:{}", pubkey.to_hex());

    if let Ok(Some(cached)) = redis_store::get_cached_string(pool, &cache_key).await {
        if let Ok(profile) = serde_json::from_str::<ProfileCache>(&cached) {
            if profile.is_valid(config.profile_cache_ttl_secs) {
                info!(pubkey = %pubkey, "Profile cache hit");
                return profile.display_name;
            }
        }
    }

    info!(pubkey = %pubkey, "Profile cache miss, querying relays");

    let filter = Filter::new()
        .kind(Kind::Metadata)
        .author(*pubkey)
        .limit(1);

    let timeout = std::time::Duration::from_secs(config.query_timeout_secs);

    let result = tokio::time::timeout(
        timeout,
        client.fetch_events(filter, timeout)
    ).await;

    if let Ok(Ok(events)) = result {
        if let Some(event) = events.first() {
            if let Ok(profile) = serde_json::from_str::<serde_json::Value>(&event.content) {
                let name = profile.get("display_name")
                    .or_else(|| profile.get("name"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                if let Some(display_name) = name {
                    info!(pubkey = %pubkey, name = %display_name, "Found profile name");

                    let cache_value = ProfileCache {
                        display_name: display_name.clone(),
                        fetched_at: chrono::Utc::now().timestamp(),
                    };

                    if let Ok(json) = serde_json::to_string(&cache_value) {
                        let _ = redis_store::set_cached_string(
                            pool,
                            &cache_key,
                            &json,
                            config.profile_cache_ttl_secs
                        ).await;
                    }

                    return display_name;
                }
            }
        }
    }

    info!(pubkey = %pubkey, "Profile query failed or no name found, using short npub");
    pubkey.to_bech32().unwrap().chars().take(20).collect()
}

async fn fetch_group_metadata(
    h_tag: &str,
    pool: &redis_store::RedisPool,
    client: &Arc<Client>,
    config: &crate::config::NotificationSettings,
) -> Option<GroupMetadata> {
    let cache_key = format!("group_meta:{}", h_tag);

    if let Ok(Some(cached)) = redis_store::get_cached_string(pool, &cache_key).await {
        if let Ok(meta) = serde_json::from_str::<GroupMetadataCache>(&cached) {
            if meta.is_valid(config.group_meta_cache_ttl_secs) {
                info!(h_tag = %h_tag, "Group metadata cache hit");
                return Some(GroupMetadata {
                    name: meta.name,
                    uuid: meta.uuid,
                });
            }
        }
    }

    info!(h_tag = %h_tag, "Group metadata cache miss, querying relay");

    let filter = Filter::new()
        .kind(Kind::Custom(39000))
        .identifier(h_tag)
        .limit(1);

    let timeout = std::time::Duration::from_secs(config.query_timeout_secs);

    let result = tokio::time::timeout(
        timeout,
        client.fetch_events(filter, timeout)
    ).await;

    if let Ok(Ok(events)) = result {
        if let Some(event) = events.first() {
            let name = event.tags.find(TagKind::Name)
                .and_then(|tag| tag.content())
                .map(|s| s.to_string())?;

            let uuid = event.tags.iter()
                .find(|t| {
                    t.kind() == TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::I)) &&
                    t.content().map(|c| c.starts_with("peek:uuid:")).unwrap_or(false)
                })
                .and_then(|tag| tag.content())
                .and_then(|s| s.strip_prefix("peek:uuid:"))
                .map(|s| s.to_string())?;

            debug!(h_tag = %h_tag, name = %name, uuid = %uuid, "Found group metadata");

            let metadata = GroupMetadata {
                name: name.clone(),
                uuid: uuid.clone(),
            };

            let cache_value = GroupMetadataCache {
                name,
                uuid,
                fetched_at: chrono::Utc::now().timestamp(),
            };

            if let Ok(json) = serde_json::to_string(&cache_value) {
                let _ = redis_store::set_cached_string(
                    pool,
                    &cache_key,
                    &json,
                    config.group_meta_cache_ttl_secs
                ).await;
            }

            return Some(metadata);
        }
    }

    info!(h_tag = %h_tag, "Group metadata query failed or not found");
    None
}

async fn create_fcm_payload(
    event: &Event,
    target_pubkey: &PublicKey,
    state: &AppState,
) -> Result<FcmPayload> {
    let mut data = std::collections::HashMap::new();

    let h_tag = event.tags.find(TagKind::h())
        .and_then(|tag| tag.content())
        .map(|s| s.to_string());

    let (sender_name, group_metadata) = if let Some(ref config) = state.notification_config {
        // Use profile_client for sender name (user profiles on public relays)
        let profile_client = state.profile_client.clone();

        let sender_name = fetch_sender_name(
            &event.pubkey,
            &state.redis_pool,
            &profile_client,
            config
        ).await;

        // Use nip29_client for group metadata (groups on NIP-29 relay)
        let group_metadata = if event.kind == Kind::Custom(9) && h_tag.is_some() {
            let nip29_client = state.nip29_client.client();
            fetch_group_metadata(
                h_tag.as_ref().unwrap(),
                &state.redis_pool,
                &nip29_client,
                config
            ).await
        } else {
            None
        };

        (sender_name, group_metadata)
    } else {
        (
            event.pubkey.to_bech32().unwrap().chars().take(20).collect(),
            None
        )
    };

    let community_name = group_metadata.as_ref()
        .map(|m| m.name.clone())
        .unwrap_or_else(|| {
            if event.kind == Kind::Custom(9) {
                "Peek".to_string()
            } else {
                let receiver = target_pubkey.to_bech32().unwrap().chars().take(12).collect::<String>();
                format!("Message to {}", receiver)
            }
        });

    let title = match event.kind {
        Kind::Custom(9) => community_name.clone(),
        Kind::GiftWrap => "New encrypted message".to_string(),
        _ => {
            let sender_short = event.pubkey.to_bech32().unwrap().chars().take(12).collect::<String>();
            let receiver_short = target_pubkey.to_bech32().unwrap().chars().take(12).collect::<String>();
            format!("New message from {} → {}", sender_short, receiver_short)
        }
    };

    let body: String = if event.kind == Kind::GiftWrap {
        "You have a new encrypted message".to_string()
    } else {
        format!(
            "{}: {}",
            sender_name,
            event.content.chars().take(150).collect::<String>()
        )
    };

    data.insert("nostrEventId".to_string(), event.id.to_hex());
    data.insert("title".to_string(), title);
    data.insert("body".to_string(), body);
    data.insert("senderPubkey".to_string(), event.pubkey.to_hex());
    data.insert("senderName".to_string(), sender_name);
    data.insert("receiverPubkey".to_string(), target_pubkey.to_hex());
    data.insert("receiverNpub".to_string(), target_pubkey.to_bech32().unwrap());
    data.insert("eventKind".to_string(), event.kind.as_u16().to_string());
    data.insert("timestamp".to_string(), event.created_at.as_u64().to_string());
    data.insert("communityName".to_string(), community_name);
    data.insert("serviceWorkerScope".to_string(), "pending".to_string());

    if let Some(h_tag_str) = h_tag {
        data.insert("groupId".to_string(), h_tag_str);
    }

    if let Some(metadata) = group_metadata {
        data.insert("communityId".to_string(), metadata.uuid);
    }

    Ok(FcmPayload {
        notification: None,
        data: Some(data),
        android: None,
        webpush: None,
        apns: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use nostr_sdk::prelude::{EventBuilder, Keys, Kind, SecretKey, Tag, Timestamp};

    // TODO: Update this test to use mock AppState with notification_config
    #[tokio::test]
    #[ignore]
    async fn test_create_fcm_payload_full() {
        // Use a fixed secret key for deterministic pubkey and event ID
        let sk =
            SecretKey::from_hex("0000000000000000000000000000000000000000000000000000000000000001")
                .unwrap();
        let keys = Keys::new(sk);

        let content = "This is a test group message mentioning someone. It has more than 150 characters to ensure that the truncation logic is tested properly. Let's add even more text here to be absolutely sure that it exceeds the limit significantly.";

        let kind = Kind::Custom(11);
        let fixed_timestamp = Timestamp::from(0); // Use fixed timestamp for deterministic event ID
        let group_id = "test_group_id"; // Define a group ID for the test

        // Create the ["h", <group_id>] tag correctly
        let h_tag =
            Tag::parse(["h".to_string(), group_id.to_string()]).expect("Failed to parse h tag");

        let test_event = EventBuilder::new(kind, content)
            .tag(h_tag) // Use .tag()
            .custom_created_at(fixed_timestamp)
            .sign(&keys)
            .await
            .unwrap();

        // Get the actual event ID after signing
        let actual_event_id = test_event.id.to_hex();

        // Create a target pubkey for the test
        let target_keys = Keys::generate();
        let target_pubkey = target_keys.public_key();
        
        // TODO: This test needs a mock AppState - skipping for now
        // let payload = create_fcm_payload(&test_event, &target_pubkey, &mock_state).await.unwrap();
    }
}
