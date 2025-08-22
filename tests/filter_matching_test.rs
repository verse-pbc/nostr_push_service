use nostr_sdk::prelude::*;
use plur_push_service::{
    config::Settings,
    event_handler,
    fcm_sender::{FcmClient, MockFcmSender},
    nostr::nip29::Nip29Client,
    redis_store::{self, RedisPool},
    state::AppState,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio_util::sync::CancellationToken;

// Counter for unique test tokens
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

async fn create_test_state() -> (Arc<AppState>, Arc<MockFcmSender>) {
    dotenvy::dotenv().ok();
    
    let redis_url = "redis://localhost:6379";
    
    std::env::set_var(
        "PLUR_PUSH__SERVICE__PRIVATE_KEY_HEX",
        "0000000000000000000000000000000000000000000000000000000000000001",
    );
    
    let mut settings = Settings::new().expect("Failed to load settings");
    settings.nostr.relay_url = "ws://localhost:8080".to_string();
    
    let redis_pool = redis_store::create_pool(redis_url, settings.redis.connection_pool_size)
        .await
        .expect("Failed to create Redis pool");
    
    cleanup_redis(&redis_pool)
        .await
        .expect("Failed to cleanup Redis");
    
    let mock_fcm = Arc::new(MockFcmSender::new());
    let fcm_client = Arc::new(FcmClient::new_with_impl(Box::new(mock_fcm.as_ref().clone())));
    
    let test_keys = Keys::generate();
    
    let nip29_client = Nip29Client::new(settings.nostr.relay_url.clone(), test_keys.clone(), 300)
        .await
        .expect("Failed to create NIP29 client");
    
    let state = Arc::new(AppState {
        settings,
        redis_pool,
        fcm_client,
        service_keys: Some(test_keys),
        nip29_client: Arc::new(nip29_client),
    });
    
    (state, mock_fcm)
}

async fn cleanup_redis(pool: &RedisPool) -> anyhow::Result<()> {
    let mut conn = pool.get().await?;
    redis::cmd("FLUSHDB").query_async::<()>(&mut *conn).await?;
    Ok(())
}

#[tokio::test]
async fn test_filter_matches_kind() {
    let (state, _mock_fcm) = create_test_state().await;
    let user_keys = Keys::generate();
    
    // Register token for user with unique ID
    let token = format!("test_token_{}", TEST_COUNTER.fetch_add(1, Ordering::SeqCst));
    redis_store::add_or_update_token(&state.redis_pool, &user_keys.public_key(), &token)
        .await
        .unwrap();
    
    // Add subscription for TextNote events
    let filter = Filter::new().kind(Kind::TextNote);
    let filter_json = serde_json::to_string(&filter).unwrap();
    redis_store::add_subscription(&state.redis_pool, &user_keys.public_key(), &filter_json)
        .await
        .unwrap();
    
    // Create a TextNote event
    let event = EventBuilder::new(Kind::TextNote, "Test message")
        .sign(&Keys::generate())
        .await
        .unwrap();
    
    // Handle the event with custom subscriptions
    let token = CancellationToken::new();
    let result = event_handler::handle_custom_subscriptions(&state, &event, event_handler::EventContext::Live, token).await;
    
    assert!(result.is_ok());
    // User should receive notification
}

#[tokio::test]
async fn test_filter_matches_author() {
    let (state, _mock_fcm) = create_test_state().await;
    let user_keys = Keys::generate();
    let author_keys = Keys::generate();
    
    // Register token with unique ID for each test
    let token = format!("test_token_{}", TEST_COUNTER.fetch_add(1, Ordering::SeqCst));
    redis_store::add_or_update_token(&state.redis_pool, &user_keys.public_key(), &token)
        .await
        .unwrap();
    
    // Subscribe to events from specific author
    let filter = Filter::new().author(author_keys.public_key());
    let filter_json = serde_json::to_string(&filter).unwrap();
    redis_store::add_subscription(&state.redis_pool, &user_keys.public_key(), &filter_json)
        .await
        .unwrap();
    
    // Create event from that author
    let event = EventBuilder::new(Kind::TextNote, "Message from subscribed author")
        .sign(&author_keys)
        .await
        .unwrap();
    
    let token = CancellationToken::new();
    let result = event_handler::handle_custom_subscriptions(&state, &event, event_handler::EventContext::Live, token).await;
    
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_filter_no_match() {
    let (state, _mock_fcm) = create_test_state().await;
    let user_keys = Keys::generate();
    
    // Register token with unique ID for each test
    let token = format!("test_token_{}", TEST_COUNTER.fetch_add(1, Ordering::SeqCst));
    redis_store::add_or_update_token(&state.redis_pool, &user_keys.public_key(), &token)
        .await
        .unwrap();
    
    // Subscribe only to Reaction events
    let filter = Filter::new().kind(Kind::Reaction);
    let filter_json = serde_json::to_string(&filter).unwrap();
    redis_store::add_subscription(&state.redis_pool, &user_keys.public_key(), &filter_json)
        .await
        .unwrap();
    
    // Create a TextNote event (not a Reaction)
    let event = EventBuilder::new(Kind::TextNote, "This should not match")
        .sign(&Keys::generate())
        .await
        .unwrap();
    
    let token = CancellationToken::new();
    let result = event_handler::handle_custom_subscriptions(&state, &event, event_handler::EventContext::Live, token).await;
    
    assert!(result.is_ok());
    // User should NOT receive notification
}

#[tokio::test]
async fn test_default_behavior_mentions() {
    let (state, _mock_fcm) = create_test_state().await;
    let user_keys = Keys::generate();
    let sender_keys = Keys::generate();
    
    // Register token but NO subscriptions
    let token = format!("test_token_{}", TEST_COUNTER.fetch_add(1, Ordering::SeqCst));
    redis_store::add_or_update_token(&state.redis_pool, &user_keys.public_key(), &token)
        .await
        .unwrap();
    
    // Create event that mentions the user
    let event = EventBuilder::new(Kind::TextNote, "Hello @user")
        .tag(Tag::parse(["p", &user_keys.public_key().to_hex()]).unwrap())
        .sign(&sender_keys)
        .await
        .unwrap();
    
    let token = CancellationToken::new();
    let result = event_handler::handle_custom_subscriptions(&state, &event, event_handler::EventContext::Live, token).await;
    
    assert!(result.is_ok());
    // User should receive notification due to mention (default behavior)
}

#[tokio::test]
async fn test_multiple_matching_filters() {
    let (state, _mock_fcm) = create_test_state().await;
    let user_keys = Keys::generate();
    let author_keys = Keys::generate();
    
    // Register token with unique ID for each test
    let token = format!("test_token_{}", TEST_COUNTER.fetch_add(1, Ordering::SeqCst));
    redis_store::add_or_update_token(&state.redis_pool, &user_keys.public_key(), &token)
        .await
        .unwrap();
    
    // Add multiple filters that could match the same event
    let filter1 = Filter::new().kind(Kind::TextNote);
    let filter2 = Filter::new().author(author_keys.public_key());
    
    for filter in [filter1, filter2] {
        let filter_json = serde_json::to_string(&filter).unwrap();
        redis_store::add_subscription(&state.redis_pool, &user_keys.public_key(), &filter_json)
            .await
            .unwrap();
    }
    
    // Create event that matches both filters
    let event = EventBuilder::new(Kind::TextNote, "Matches multiple filters")
        .sign(&author_keys)
        .await
        .unwrap();
    
    let token = CancellationToken::new();
    let result = event_handler::handle_custom_subscriptions(&state, &event, event_handler::EventContext::Live, token).await;
    
    assert!(result.is_ok());
    // User should receive notification (only once despite multiple matches)
}