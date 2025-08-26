use crate::{
    // config::CONFIG,
    error::Result,
    // fcm::{send_fcm_message, FcmPayload},
    fcm_sender,
    models::FcmPayload,
    nostr::nip29,
    redis_store,
    state::AppState,
};
use nostr_sdk::prelude::*;
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
const KIND_DM: Kind = Kind::Custom(1059); // NIP-17 Private Direct Messages
const BROADCASTABLE_EVENT_KINDS: [Kind; 1] = [Kind::Custom(9)]; // Only kind 9 is broadcastable
// Note: Group messages are now identified by presence of 'h' tag, not by specific kinds

// Replay horizon: ignore events older than this
const REPLAY_HORIZON_DAYS: u64 = 7;

pub async fn run(
    state: Arc<AppState>,
    mut event_rx: Receiver<(Box<Event>, EventContext)>,
    token: CancellationToken,
) -> Result<()> {
    tracing::info!("Starting event handler...");

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

                debug!(event_id = %event_id, kind = %event_kind, "Dispatching event handler");

                let handler_result = if event_kind == KIND_REGISTRATION {
                    handle_registration(&state, &event).await
                } else if event_kind == KIND_DEREGISTRATION {
                    handle_deregistration(&state, &event).await
                } else if event_kind == KIND_SUBSCRIPTION_UPSERT {
                    handle_subscription_upsert(&state, &event, token.clone()).await
                } else if event_kind == KIND_SUBSCRIPTION_DELETE {
                    handle_subscription_delete(&state, &event, token.clone()).await
                } else if event_kind == KIND_DM {
                    handle_dm(&state, &event, token.clone()).await
                } else if event.tags.find(TagKind::h()).is_some() {
                    // Handle events with 'h' tag (group-scoped events)
                    // This includes group messages, replies, and any other group-scoped events
                    // Note: We only run the group handler, not custom subscriptions, to avoid duplicate notifications
                    // for mentions that are already handled by the group logic
                    handle_group_message(&state, &event, token.clone()).await
                } else {
                    // For all other events (including non-group kind 9/10), check user subscriptions
                    handle_custom_subscriptions(&state, &event, context, token.clone()).await
                };

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
                        // Removed downcast_ref check for ServiceError::Cancelled
                        // Cancellation is handled by the select! blocks
                        /*
                        if let Some(service_error) = e.downcast_ref::<crate::error::ServiceError>() {
                            if matches!(service_error, crate::error::ServiceError::Cancelled) {
                                info!(event_id = %event_id, "Handler for event cancelled internally.");
                                break; // Exit outer loop if handler was cancelled
                            }
                        }
                        */
                        error!(event_id = %event_id, error = %e, "Failed to handle event");
                        // Decide if the error is fatal or if we should continue processing other events
                        // For now, continue processing other events
                    }
                }

                if token.is_cancelled() {
                    info!(event_id = %event_id, "Event handler cancellation detected after processing event {}.", event_id);
                    break;
                }
            }
        }
    }

    info!("Event handler shut down.");
    Ok(())
}

async fn handle_registration(state: &AppState, event: &Event) -> Result<()> {
    assert!(event.kind == KIND_REGISTRATION);

    let fcm_token = event.content.trim();
    if fcm_token.is_empty() {
        warn!(
            event_id = %event.id, pubkey = %event.pubkey,
            "Received registration event with empty token"
        );
        return Ok(());
    }

    match redis_store::add_or_update_token(&state.redis_pool, &event.pubkey, fcm_token).await {
        Ok(_) => {
        }
        Err(e) => {
            return Err(e);
        }
    }
    info!(event_id = %event.id, pubkey = %event.pubkey, "Registered/Updated token");
    Ok(())
}

async fn handle_deregistration(state: &AppState, event: &Event) -> Result<()> {
    assert!(event.kind == KIND_DEREGISTRATION);

    let fcm_token = event.content.trim();
    if fcm_token.is_empty() {
        warn!(
            event_id = %event.id, pubkey = %event.pubkey,
            "Received deregistration event with empty token"
        );
        return Ok(());
    }

    let removed = redis_store::remove_token(&state.redis_pool, &event.pubkey, fcm_token).await?;
    if removed {
        info!(event_id = %event.id, pubkey = %event.pubkey, "Deregistered token");
    } else {
        debug!(
            event_id = %event.id, pubkey = %event.pubkey, token_prefix = &fcm_token[..8.min(fcm_token.len())],
            "Token not found for deregistration"
        );
    }
    Ok(())
}

/// Handle DM events (kind 1059) by notifying all recipients in p-tags
pub async fn handle_dm(state: &AppState, event: &Event, token: CancellationToken) -> Result<()> {
    debug!(event_id = %event.id, "Handling DM event");

    if token.is_cancelled() {
        info!(event_id = %event.id, "Cancelled before handling DM.");
        return Err(crate::error::ServiceError::Cancelled);
    }

    // Extract all p-tags (recipients)
    let recipients: Vec<PublicKey> = event
        .tags
        .iter()
        .filter(|t| t.kind() == TagKind::p())
        .filter_map(|t| t.content())
        .filter_map(|content| PublicKey::from_str(content).ok())
        .collect();

    debug!(event_id = %event.id, recipient_count = recipients.len(), "Found DM recipients");

    if recipients.is_empty() {
        debug!(event_id = %event.id, "No recipients found in DM p-tags");
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

        trace!(event_id = %event.id, recipient = %recipient_pubkey, "Processing DM for recipient");

        if let Err(e) =
            send_notification_to_user(state, event, &recipient_pubkey, token.clone()).await
        {
            if matches!(e, crate::error::ServiceError::Cancelled) {
                return Err(e);
            }
            error!(event_id = %event.id, recipient = %recipient_pubkey, error = %e, "Failed to send DM notification");
        }
    }

    debug!(event_id = %event.id, "Finished handling DM");
    Ok(())
}

/// Handle subscription upsert events (kind 3081)
pub async fn handle_subscription_upsert(
    state: &AppState,
    event: &Event,
    token: CancellationToken,
) -> Result<()> {
    debug!(event_id = %event.id, "Handling subscription upsert");

    if token.is_cancelled() {
        info!(event_id = %event.id, "Cancelled before handling subscription upsert.");
        return Err(crate::error::ServiceError::Cancelled);
    }

    // Parse the filter from the event content
    let filter: Filter = match serde_json::from_str(&event.content) {
        Ok(f) => f,
        Err(e) => {
            warn!(event_id = %event.id, error = %e, "Invalid filter JSON in subscription event");
            return Ok(()); // Don't fail the whole handler for invalid JSON
        }
    };

    // Normalize the filter JSON for consistent storage
    let filter_json = serde_json::to_string(&filter).map_err(|e| {
        crate::error::ServiceError::Internal(format!("Failed to serialize filter: {}", e))
    })?;

    // Store the subscription with the current timestamp
    let subscription_timestamp = event.created_at.as_u64();
    redis_store::add_subscription_with_timestamp(
        &state.redis_pool, 
        &event.pubkey, 
        &filter_json,
        subscription_timestamp
    ).await?;

    info!(
        event_id = %event.id, 
        pubkey = %event.pubkey, 
        timestamp = subscription_timestamp,
        "Added/updated subscription with timestamp"
    );
    Ok(())
}

/// Handle subscription delete events (kind 3082)
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

    // Parse the filter from the event content
    let filter: Filter = match serde_json::from_str(&event.content) {
        Ok(f) => f,
        Err(e) => {
            warn!(event_id = %event.id, error = %e, "Invalid filter JSON in subscription delete event");
            return Ok(()); // Don't fail the whole handler for invalid JSON
        }
    };

    // Normalize the filter JSON for consistent removal
    let filter_json = serde_json::to_string(&filter).map_err(|e| {
        crate::error::ServiceError::Internal(format!("Failed to serialize filter: {}", e))
    })?;

    // Remove the subscription
    redis_store::remove_subscription(&state.redis_pool, &event.pubkey, &filter_json).await?;

    info!(event_id = %event.id, pubkey = %event.pubkey, "Removed subscription");
    Ok(())
}

/// Handle group-scoped events (events with 'h' tag) by checking mentions and group membership.
/// For broadcast messages, notify all group members.
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

    // Continue with regular mention-based notification logic
    let mentioned_pubkeys = extract_mentioned_pubkeys(event);
    debug!(event_id = %event.id, mentions = mentioned_pubkeys.len(), "Extracted mentioned pubkeys");

    if mentioned_pubkeys.is_empty() {
        debug!(event_id = %event.id, "No mentioned pubkeys found, skipping notification.");
        return Ok(());
    }

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

    let tokens = tokio::select! {
        biased;
        _ = token.cancelled() => {
            info!(event_id = %event_id, target_pubkey = %target_pubkey, "Cancelled while fetching tokens.");
            return Err(crate::error::ServiceError::Cancelled);
        }
        res = redis_store::get_tokens_for_pubkey(&state.redis_pool, target_pubkey) => {
            res?
        }
    };
    trace!(event_id = %event_id, target_pubkey = %target_pubkey, count = tokens.len(), "Found tokens");

    if tokens.is_empty() {
        trace!(event_id = %event_id, target_pubkey = %target_pubkey, "No registered tokens found, skipping.");
        return Ok(());
    }

    trace!(event_id = %event_id, target_pubkey = %target_pubkey, "Creating FCM payload");
    let payload = create_fcm_payload(event, target_pubkey)?;

    trace!(event_id = %event_id, target_pubkey = %target_pubkey, token_count = tokens.len(), "Attempting to send FCM notification");

    let results = tokio::select! {
        biased;
        _ = token.cancelled() => {
            info!(event_id = %event_id, target_pubkey = %target_pubkey, "Cancelled during FCM send batch.");
            return Err(crate::error::ServiceError::Cancelled);
        }
        send_result = state.fcm_client.send_batch(&tokens, payload) => {
            send_result
        }
    };
    trace!(event_id = %event_id, target_pubkey = %target_pubkey, results_count = results.len(), "FCM send completed");

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
    debug!(event_id = %event_id, target_pubkey = %target_pubkey, success = success_count, removed = tokens_to_remove.len(), "FCM send summary");

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

/// Handle events based on user-defined subscription filters
pub async fn handle_custom_subscriptions(
    state: &AppState,
    event: &Event,
    context: EventContext,
    token: CancellationToken,
) -> Result<()> {
    debug!(event_id = %event.id, kind = %event.kind, "Handling event with custom subscriptions");

    if token.is_cancelled() {
        info!(event_id = %event.id, "Cancelled before handling custom subscriptions.");
        return Err(crate::error::ServiceError::Cancelled);
    }

    // Track which users we've already processed to avoid duplicates
    let mut processed_users = std::collections::HashSet::new();

    // First, check users with subscriptions
    let users_with_subscriptions =
        redis_store::get_all_users_with_subscriptions(&state.redis_pool).await?;

    for user_pubkey in users_with_subscriptions {
        if token.is_cancelled() {
            info!(event_id = %event.id, "Cancelled during custom subscription processing");
            return Err(crate::error::ServiceError::Cancelled);
        }

        // Skip if this is the sender (don't notify user of their own messages)
        if user_pubkey == event.pubkey {
            info!(
                event_id = %event.id, 
                user_npub = %user_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()),
                sender_npub = %event.pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()),
                "Skipping notification for sender in custom subscriptions"
            );
            continue;
        }

        processed_users.insert(user_pubkey);

        // Skip if user has no tokens
        let tokens = redis_store::get_tokens_for_pubkey(&state.redis_pool, &user_pubkey).await?;
        if tokens.is_empty() {
            continue;
        }

        // Get user's subscriptions with timestamps
        let subscriptions_with_timestamps = redis_store::get_subscriptions_with_timestamps(&state.redis_pool, &user_pubkey).await?;

        // Check if event matches any subscription filter AND was created after subscription
        let mut matched = false;
        let event_timestamp = event.created_at.as_u64();
        
        for (filter_json, subscription_timestamp) in subscriptions_with_timestamps {
            // For Live events: always check timestamps - don't send notifications for events that occurred before subscription
            // For Historical events: also check timestamps during replay
            // Use strict > (i.e., skip if <=) as per best practice to avoid edge duplicates
            if event_timestamp <= subscription_timestamp {
                trace!(
                    event_id = %event.id, 
                    user = %user_pubkey, 
                    event_time = event_timestamp,
                    subscription_time = subscription_timestamp,
                    context = ?context,
                    "Event predates subscription, skipping"
                );
                continue;
            }
            
            let filter: Filter = match serde_json::from_str(&filter_json) {
                Ok(f) => f,
                Err(e) => {
                    warn!(user = %user_pubkey, error = %e, "Invalid filter JSON in storage");
                    continue;
                }
            };

            if filter.match_event(event) {
                trace!(event_id = %event.id, user = %user_pubkey, "Event matches subscription filter and timestamp");
                matched = true;
                break; // One match is enough
            }
        }

        if matched {
            info!(
                event_id = %event.id,
                user_npub = %user_pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()),
                sender_npub = %event.pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()),
                "Sending notification via custom subscription"
            );
            if let Err(e) =
                send_notification_to_user(state, event, &user_pubkey, token.clone()).await
            {
                if matches!(e, crate::error::ServiceError::Cancelled) {
                    return Err(e);
                }
                error!(event_id = %event.id, user = %user_pubkey, error = %e, "Failed to send notification");
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

        // Check if user has tokens (meaning they've registered for push)
        let tokens =
            redis_store::get_tokens_for_pubkey(&state.redis_pool, &mentioned_pubkey).await?;
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

fn create_fcm_payload(event: &Event, target_pubkey: &PublicKey) -> Result<FcmPayload> {
    let sender = &event
        .pubkey
        .to_bech32()
        .unwrap()
        .chars()
        .take(12)
        .collect::<String>();
    
    let receiver = &target_pubkey
        .to_bech32()
        .unwrap()
        .chars()
        .take(12)
        .collect::<String>();

    let title = match event.kind {
        Kind::Custom(9) => format!("Chat from {} → {}", sender, receiver),
        Kind::Custom(1059) => format!("Private message for {}", receiver),
        _ => format!("New message from {} → {}", sender, receiver),
    };

    let body: String = if event.kind == Kind::Custom(1059) {
        "You have a new encrypted message".to_string()
    } else {
        event.content.chars().take(150).collect()
    };

    let mut data = std::collections::HashMap::new();
    data.insert("nostrEventId".to_string(), event.id.to_hex());

    // Add notification content to data payload for service worker to use
    data.insert("title".to_string(), title.clone());
    data.insert("body".to_string(), body.clone());
    data.insert("senderPubkey".to_string(), event.pubkey.to_hex());
    data.insert("receiverPubkey".to_string(), target_pubkey.to_hex());
    data.insert("receiverNpub".to_string(), target_pubkey.to_bech32().unwrap());
    data.insert("eventKind".to_string(), event.kind.as_u16().to_string());
    data.insert(
        "timestamp".to_string(),
        event.created_at.as_u64().to_string(),
    );
    
    // Add service worker debug info - this will be populated by the service worker
    data.insert("serviceWorkerScope".to_string(), "pending".to_string());

    // Extract group ID from 'h' tag using find() and content()
    let group_id = event
        .tags
        .find(TagKind::h()) // Find the first raw Tag with kind 'h'
        .and_then(|tag| tag.content()); // Get the content (value at index 1)

    if let Some(id_str) = group_id {
        // id_str is Option<&str>, convert to String for insertion
        data.insert("groupId".to_string(), id_str.to_string());
    }

    // Add other relevant event details here if needed by the client app.
    // These will be sent in the top-level `data` field of the FCM message.

    Ok(FcmPayload {
        // Basic cross-platform notification fields.
        // These are mapped to `firebase_messaging_rs::fcm::Notification`.
        // Send as data-only for full control over notification display
        notification: None,
        // Arbitrary key-value data for the client application.
        // This is mapped to the `data` field in `firebase_messaging_rs::fcm::Message::Token`.
        data: Some(data),
        // Platform-specific configurations (currently NOT mapped/used by fcm_sender.rs).
        // These fields exist in FcmPayload (as defined in models.rs) to align
        // with the potential structure of the FCM v1 API message object, but the
        // current implementation in fcm_sender.rs only uses .notification and .data.
        // To use these, the mapping logic in fcm_sender.rs would need enhancement.
        android: None, // Example: Populate with serde_json::Value if needed.
        webpush: None, // Example: Populate with serde_json::Value if needed.
        apns: None,    // Example: Populate with serde_json::Value if needed.
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use nostr_sdk::prelude::{EventBuilder, Keys, Kind, SecretKey, Tag, Timestamp};

    #[tokio::test]
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
        
        let payload_result = create_fcm_payload(&test_event, &target_pubkey);
        assert!(payload_result.is_ok());
        let payload = payload_result.unwrap();

        // Serialize the actual payload to JSON string
        let actual_json = serde_json::to_string_pretty(&payload).unwrap();

        // Define the expected JSON using the group_id and the actual_event_id
        // Now expects data-only message (no notification field)
        let receiver_npub_short = target_pubkey.to_bech32().unwrap().chars().take(12).collect::<String>();
        let expected_json = format!(
            r#"{{
            "data": {{
              "nostrEventId": "{}",
              "title": "New message from npub10xlxvlh → {}",
              "body": "This is a test group message mentioning someone. It has more than 150 characters to ensure that the truncation logic is tested properly. Let's add eve",
              "senderPubkey": "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
              "eventKind": "11",
              "timestamp": "0",
              "groupId": "{}",
              "receiverPubkey": "{}",
              "receiverNpub": "{}",
              "serviceWorkerScope": "pending"
            }}
          }}"#,
            actual_event_id, 
            receiver_npub_short,
            group_id,
            target_pubkey.to_hex(),
            target_pubkey.to_bech32().unwrap()
        );

        // Parse both JSON strings back into serde_json::Value for comparison
        let actual_value: serde_json::Value = serde_json::from_str(&actual_json).unwrap();
        let expected_value: serde_json::Value = serde_json::from_str(&expected_json).unwrap();

        // Compare the serde_json::Value objects
        assert_eq!(actual_value, expected_value);
    }
}
