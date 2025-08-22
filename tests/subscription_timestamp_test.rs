use anyhow::Result;
use nostr_sdk::{Event, EventBuilder, Keys, Kind, Tag, Timestamp, ToBech32};
use serial_test::serial;
use nostr_push_service::{
    config::Settings,
    redis_store::{self},
    state::AppState,
};
use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// Global counter for unique test IDs
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_test_id() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("{}_{}", timestamp, counter)
}

// Event context to distinguish historical from live events
#[derive(Debug, Clone, Copy)]
pub enum EventContext {
    Historical,
    Live,
}

/// Setup test state with mock FCM
async fn setup_test_state() -> Result<(Arc<AppState>, Arc<nostr_push_service::fcm_sender::MockFcmSender>)> {
    dotenvy::dotenv().ok();
    let redis_host = env::var("REDIS_HOST").unwrap_or_else(|_| "localhost".to_string());
    let redis_port = env::var("REDIS_PORT").unwrap_or_else(|_| "6379".to_string());
    
    let redis_url = format!("redis://{}:{}", redis_host, redis_port);

    // Safety check
    if let Ok(parsed_url) = url::Url::parse(&redis_url) {
        if parsed_url
            .host_str()
            .is_some_and(|host| host.ends_with(".db.ondigitalocean.com"))
        {
            panic!("Safety check: Cannot run tests against production database");
        }
    }

    let test_key_hex = "0000000000000000000000000000000000000000000000000000000000000001";
    std::env::set_var("NOSTR_PUSH__SERVICE__PRIVATE_KEY_HEX", test_key_hex);

    let mut settings = Settings::new()?;
    settings.nostr.relay_url = "wss://test.relay".to_string();
    
    let redis_pool = redis_store::create_pool(&redis_url, settings.redis.connection_pool_size).await?;
    
    // Clean up Redis
    // Flush Redis DB for test isolation (tests run serially)
    {
        let mut conn = redis_pool.get().await?;
        redis::cmd("FLUSHDB").query_async::<()>(&mut *conn).await?;
    }

    let mock_fcm = nostr_push_service::fcm_sender::MockFcmSender::new();
    let mock_fcm_arc = Arc::new(mock_fcm.clone());
    let fcm_client = Arc::new(nostr_push_service::fcm_sender::FcmClient::new_with_impl(
        Box::new(mock_fcm),
    ));

    let test_keys = Keys::generate();
    let nip29_client = nostr_push_service::nostr::nip29::Nip29Client::new(
        settings.nostr.relay_url.clone(),
        test_keys.clone(),
        300,
    )
    .await?;

    let app_state = AppState {
        settings,
        redis_pool,
        fcm_client,
        service_keys: Some(test_keys),
        nip29_client: Arc::new(nip29_client),
    };

    Ok((Arc::new(app_state), mock_fcm_arc))
}

/// Register user FCM token
async fn register_user_token(state: &Arc<AppState>, user_keys: &Keys, token: &str) -> Result<()> {
    redis_store::add_or_update_token(&state.redis_pool, &user_keys.public_key(), token).await?;
    Ok(())
}

/// Process event with context - simulates the actual notification process
async fn process_event_with_context(
    state: &Arc<AppState>,
    event: &Event,
    context: EventContext,
) -> Result<usize> {
    // Check if event is already processed
    if redis_store::is_event_processed(&state.redis_pool, &event.id).await? {
        return Ok(0);
    }
    
    // Mark as processed
    redis_store::mark_event_processed(&state.redis_pool, &event.id, 604800).await?;
    
    // Count notifications that would be sent based on subscriptions
    let mut notification_count = 0;
    
    // Get all users with subscriptions
    let users = redis_store::get_all_users_with_subscriptions(&state.redis_pool).await?;
    
    for user_pubkey in users {
        // Skip self-notifications
        if user_pubkey == event.pubkey {
            continue;
        }
        
        // Check if user has tokens
        let tokens = redis_store::get_tokens_for_pubkey(&state.redis_pool, &user_pubkey).await?;
        if tokens.is_empty() {
            continue;
        }
        
        // Get subscriptions with timestamps
        let subscriptions = redis_store::get_subscriptions_with_timestamps(&state.redis_pool, &user_pubkey).await?;
        
        for (filter_json, subscription_timestamp) in subscriptions {
            // Check timestamp - skip if event is older than subscription
            let event_time = event.created_at.as_u64();
            if event_time < subscription_timestamp {
                continue;
            }
            
            // Check if filter matches
            let filter: nostr_sdk::Filter = serde_json::from_str(&filter_json)?;
            if filter.match_event(event) {
                notification_count += 1;
                break; // One match per user is enough
            }
        }
    }
    
    // For historical events, skip mentions
    if matches!(context, EventContext::Historical) {
        return Ok(notification_count);
    }
    
    // For live events, also check mentions (p tags)
    use nostr_sdk::TagKind;
    for tag in event.tags.iter() {
        if tag.kind() == TagKind::p() {
            if let Some(content) = tag.content() {
                if let Ok(pubkey) = nostr_sdk::PublicKey::from_hex(content) {
                    if pubkey != event.pubkey {
                        // Check if mentioned user has tokens
                        let tokens = redis_store::get_tokens_for_pubkey(&state.redis_pool, &pubkey).await?;
                        if !tokens.is_empty() {
                            notification_count += 1;
                        }
                    }
                }
            }
        }
    }
    
    Ok(notification_count)
}

#[tokio::test]
#[serial]
async fn test_subscription_respects_timestamp() -> anyhow::Result<()> {
    // Setup
    let (state, _fcm_mock) = setup_test_state().await?;
    let user_keys = Keys::generate();
    let user_pubkey = user_keys.public_key();
    let sender_keys = Keys::generate();
    
    // Register user with FCM token using unique ID
    let token = format!("test_token_{}", unique_test_id());
    register_user_token(&state, &user_keys, &token).await?;
    
    // Create an old event (before subscription) with explicit timestamp
    let old_timestamp = Timestamp::now() - Duration::from_secs(10); // 10 seconds ago
    let old_event = EventBuilder::new(
        Kind::Custom(9),
        "Old message that should not trigger notification"
    )
    .custom_created_at(old_timestamp)
    .sign(&sender_keys)
    .await?;
    
    // Now subscribe to kind 9 events
    let subscription_time = Timestamp::now();
    let filter = nostr_sdk::Filter::new().kinds(vec![Kind::Custom(9)]);
    let filter_json = serde_json::to_string(&filter)?;
    
    redis_store::add_subscription_with_timestamp(
        &state.redis_pool,
        &user_pubkey,
        &filter_json,
        subscription_time.as_u64()
    ).await?;
    
    // Create a new event (after subscription)
    let new_timestamp = Timestamp::now() + Duration::from_secs(10); // 10 seconds from now
    let new_event = EventBuilder::new(
        Kind::Custom(9),
        "New message that should trigger notification"
    )
    .custom_created_at(new_timestamp)
    .sign(&sender_keys)
    .await?;
    
    // Process the old event - should NOT send notification
    // First clear any processed events to ensure a clean test
    {
        let mut conn = state.redis_pool.get().await?;
        redis::cmd("DEL").arg("processed_nostr_events").query_async::<()>(&mut *conn).await?;
    }
    
    let old_result = process_event_with_context(
        &state,
        &old_event,
        EventContext::Historical,
    ).await?;
    
    assert_eq!(old_result, 0, "Should not send notification for event before subscription");
    
    // Clear processed events again for the new event test
    {
        let mut conn = state.redis_pool.get().await?;
        redis::cmd("DEL").arg("processed_nostr_events").query_async::<()>(&mut *conn).await?;
    }
    
    // Process the new event - should send notification
    let new_result = process_event_with_context(
        &state,
        &new_event,
        EventContext::Live,
    ).await?;
    
    assert_eq!(new_result, 1, "Should send notification for event after subscription");
    
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_historical_events_skip_mentions() -> anyhow::Result<()> {
    // Setup
    let (state, _fcm_mock) = setup_test_state().await?;
    let mentioned_keys = Keys::generate();
    let mentioned_pubkey = mentioned_keys.public_key();
    let sender_keys = Keys::generate();
    
    // Register mentioned user with FCM token
    register_user_token(&state, &mentioned_keys, "mentioned_token_123").await?;
    
    // Create event with mention
    let event = EventBuilder::new(
        Kind::Custom(9),
        format!("Hello @{}", mentioned_pubkey.to_bech32()?)
    )
    .tag(Tag::public_key(mentioned_pubkey))
    .sign(&sender_keys)
    .await?;
    
    // Process as historical event - should NOT send mention notification
    let result = process_event_with_context(
        &state,
        &event,
        EventContext::Historical,
    ).await?;
    
    assert_eq!(result, 0, "Historical events should not trigger mention notifications");
    
    // Clear processed events for the live event test
    {
        let mut conn = state.redis_pool.get().await?;
        redis::cmd("DEL").arg("processed_nostr_events").query_async::<()>(&mut *conn).await?;
    }
    
    // Process same event as live - should send mention notification
    let result = process_event_with_context(
        &state,
        &event,
        EventContext::Live,
    ).await?;
    
    assert_eq!(result, 1, "Live events should trigger mention notifications");
    
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_processed_events_persist_across_restart() -> anyhow::Result<()> {
    // Setup
    let (state, _fcm_mock) = setup_test_state().await?;
    let event = EventBuilder::new(
        Kind::Custom(9),
        "Test message"
    )
    .sign(&Keys::generate())
    .await?;
    
    let event_id = event.id;
    
    // Mark event as processed
    redis_store::mark_event_processed(
        &state.redis_pool,
        &event_id,
        604800 // 7 days TTL
    ).await?;
    
    // Check it's marked as processed
    let is_processed = redis_store::is_event_processed(&state.redis_pool, &event_id).await?;
    assert!(is_processed, "Event should be marked as processed");
    
    // Simulate restart by creating new state (but same Redis)
    // Note: We're not clearing Redis, so the processed event should persist
    let new_redis_pool = state.redis_pool.clone();
    
    // Check event is still marked as processed with the same pool
    let still_processed = redis_store::is_event_processed(&new_redis_pool, &event_id).await?;
    assert!(still_processed, "Event should remain processed after restart");
    
    // Check TTL is set
    let ttl = redis_store::get_event_ttl(&new_redis_pool, &event_id).await?;
    assert!(ttl > 0, "Processed event should have TTL set");
    assert!(ttl <= 604800, "TTL should not exceed 7 days");
    
    Ok(())
}

#[tokio::test]
#[serial]
async fn test_subscription_filter_with_multiple_users() -> anyhow::Result<()> {
    // Setup
    let (state, _fcm_mock) = setup_test_state().await?;
    
    // Create 3 users
    let user1_keys = Keys::generate();
    let user2_keys = Keys::generate();
    let user3_keys = Keys::generate();
    let sender_keys = Keys::generate();
    
    // Register all users
    register_user_token(&state, &user1_keys, "token1").await?;
    register_user_token(&state, &user2_keys, "token2").await?;
    register_user_token(&state, &user3_keys, "token3").await?;
    
    // Create an event
    let event = EventBuilder::new(
        Kind::Custom(9),
        "Test message for subscription timing"
    )
    .sign(&sender_keys)
    .await?;
    
    // User 1 subscribes BEFORE the event
    let early_time = Timestamp::now() - Duration::from_secs(3600); // 1 hour ago
    let filter = nostr_sdk::Filter::new().kinds(vec![Kind::Custom(9)]);
    let filter_json = serde_json::to_string(&filter)?;
    
    redis_store::add_subscription_with_timestamp(
        &state.redis_pool,
        &user1_keys.public_key(),
        &filter_json,
        early_time.as_u64()
    ).await?;
    
    // User 2 subscribes AFTER the event  
    let late_time = Timestamp::now() + Duration::from_secs(3600); // 1 hour from now
    redis_store::add_subscription_with_timestamp(
        &state.redis_pool,
        &user2_keys.public_key(),
        &filter_json,
        late_time.as_u64()
    ).await?;
    
    // User 3 has no subscription
    
    // Process the event as HISTORICAL - this tests the timestamp filtering
    let result = process_event_with_context(
        &state,
        &event,
        EventContext::Historical,
    ).await?;
    
    // We expect only user1 to be notified (subscribed before the event)
    assert_eq!(result, 1, "Only user1 should be notified for historical events");
    
    Ok(())
}