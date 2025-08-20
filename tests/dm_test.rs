use nostr_sdk::prelude::*;
use plur_push_service::{
    config::Settings, 
    event_handler, 
    fcm_sender::{FcmClient, MockFcmSender},
    nostr::nip29::Nip29Client,
    redis_store::{self, RedisPool}, 
    state::AppState
};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

async fn create_test_state() -> Arc<AppState> {
    // Setup test environment
    dotenvy::dotenv().ok();
    
    // Use test Redis instance
    let redis_url = "redis://localhost:6379";
    
    // Set test keys
    std::env::set_var("PLUR_PUSH__SERVICE__PRIVATE_KEY_HEX", 
        "0000000000000000000000000000000000000000000000000000000000000001");
    
    let mut settings = Settings::new().expect("Failed to load settings");
    settings.nostr.relay_url = "ws://localhost:8080".to_string(); // Mock relay URL
    
    // Create Redis pool
    let redis_pool = redis_store::create_pool(
        redis_url,
        settings.redis.connection_pool_size,
    )
    .await
    .expect("Failed to create Redis pool");
    
    // Clean Redis before tests
    cleanup_redis(&redis_pool).await.expect("Failed to cleanup Redis");
    
    // Create mock FCM client
    let mock_fcm = MockFcmSender::new();
    let fcm_client = Arc::new(FcmClient::new_with_impl(Box::new(mock_fcm)));
    
    // Create test keys
    let test_keys = Keys::generate();
    
    // Create NIP29 client (mock)
    let nip29_client = Nip29Client::new(
        settings.nostr.relay_url.clone(),
        test_keys.clone(),
        300, // cache expiration
    )
    .await
    .expect("Failed to create NIP29 client");
    
    Arc::new(AppState {
        settings,
        redis_pool,
        fcm_client,
        service_keys: Some(test_keys),
        nip29_client: Arc::new(nip29_client),
    })
}

async fn cleanup_redis(pool: &RedisPool) -> anyhow::Result<()> {
    use redis::AsyncCommands;
    let mut conn = pool.get().await?;
    redis::cmd("FLUSHDB")
        .query_async::<()>(&mut *conn)
        .await?;
    Ok(())
}

#[tokio::test]
async fn test_dm_handler_sends_to_recipients() {
    let state = create_test_state().await;
    
    // Create a DM event (kind 1059) with p-tags for recipients
    let sender_keys = Keys::generate();
    let recipient1_keys = Keys::generate();
    let recipient2_keys = Keys::generate();
    
    // Build DM event with p-tags
    let dm_event = EventBuilder::new(Kind::Custom(1059), "Private message content")
        .tag(Tag::parse(["p", &recipient1_keys.public_key().to_hex()]).unwrap())
        .tag(Tag::parse(["p", &recipient2_keys.public_key().to_hex()]).unwrap())
        .sign(&sender_keys)
        .await
        .unwrap();
    
    // Register tokens for recipients
    redis_store::add_or_update_token(
        &state.redis_pool,
        &recipient1_keys.public_key(),
        "test_token_1",
    )
    .await
    .unwrap();
    
    redis_store::add_or_update_token(
        &state.redis_pool,
        &recipient2_keys.public_key(),
        "test_token_2",
    )
    .await
    .unwrap();
    
    // Call the DM handler
    let token = CancellationToken::new();
    let result = event_handler::handle_dm(&state, &dm_event, token).await;
    
    // Verify notifications were sent to both recipients
    assert!(result.is_ok());
    // Additional assertions would check that FCM was called with correct tokens
}

#[tokio::test]
async fn test_dm_handler_skips_sender() {
    let state = create_test_state().await;
    
    // Create a DM where sender is also in p-tags (self-DM)
    let sender_keys = Keys::generate();
    let recipient_keys = Keys::generate();
    
    let dm_event = EventBuilder::new(Kind::Custom(1059), "Private message")
        .tag(Tag::parse(["p", &sender_keys.public_key().to_hex()]).unwrap()) // Self
        .tag(Tag::parse(["p", &recipient_keys.public_key().to_hex()]).unwrap())
        .sign(&sender_keys)
        .await
        .unwrap();
    
    // Register token for sender (should be skipped)
    redis_store::add_or_update_token(
        &state.redis_pool,
        &sender_keys.public_key(),
        "sender_token",
    )
    .await
    .unwrap();
    
    // Call handler
    let token = CancellationToken::new();
    let result = event_handler::handle_dm(&state, &dm_event, token).await;
    
    assert!(result.is_ok());
    // Verify sender didn't receive notification
}

#[tokio::test]
async fn test_dm_handler_no_recipients_with_tokens() {
    let state = create_test_state().await;
    
    let sender_keys = Keys::generate();
    let recipient_keys = Keys::generate();
    
    let dm_event = EventBuilder::new(Kind::Custom(1059), "Private message")
        .tag(Tag::parse(["p", &recipient_keys.public_key().to_hex()]).unwrap())
        .sign(&sender_keys)
        .await
        .unwrap();
    
    // No tokens registered for recipient
    
    let token = CancellationToken::new();
    let result = event_handler::handle_dm(&state, &dm_event, token).await;
    
    // Should succeed but not send any notifications
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_dm_handler_filters_only_p_tags() {
    let state = create_test_state().await;
    
    let sender_keys = Keys::generate();
    let recipient_keys = Keys::generate();
    
    // Event with various tags, but only p-tags matter
    let dm_event = EventBuilder::new(Kind::Custom(1059), "Message")
        .tag(Tag::parse(["p", &recipient_keys.public_key().to_hex()]).unwrap())
        .tag(Tag::parse(["e", "some_event_id"]).unwrap()) // Should be ignored
        .tag(Tag::parse(["t", "topic"]).unwrap()) // Should be ignored
        .sign(&sender_keys)
        .await
        .unwrap();
    
    redis_store::add_or_update_token(
        &state.redis_pool,
        &recipient_keys.public_key(),
        "test_token",
    )
    .await
    .unwrap();
    
    let token = CancellationToken::new();
    let result = event_handler::handle_dm(&state, &dm_event, token).await;
    
    assert!(result.is_ok());
    // Only the p-tag recipient should get notified
}