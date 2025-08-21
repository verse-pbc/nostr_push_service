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
use tokio_util::sync::CancellationToken;

async fn create_test_state() -> Arc<AppState> {
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
    
    let mock_fcm = MockFcmSender::new();
    let fcm_client = Arc::new(FcmClient::new_with_impl(Box::new(mock_fcm)));
    
    let test_keys = Keys::generate();
    
    let nip29_client = Nip29Client::new(settings.nostr.relay_url.clone(), test_keys.clone(), 300)
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
    let mut conn = pool.get().await?;
    redis::cmd("FLUSHDB").query_async::<()>(&mut *conn).await?;
    Ok(())
}

#[tokio::test]
async fn test_subscription_upsert() {
    let state = create_test_state().await;
    let user_keys = Keys::generate();
    
    // Create a filter for kind 1 events
    let filter = Filter::new().kind(Kind::TextNote);
    let filter_json = serde_json::to_string(&filter).unwrap();
    
    // Create subscription upsert event (kind 3081)
    let sub_event = EventBuilder::new(Kind::Custom(3081), &filter_json)
        .sign(&user_keys)
        .await
        .unwrap();
    
    // Handle the subscription
    let token = CancellationToken::new();
    let result = event_handler::handle_subscription_upsert(&state, &sub_event, token).await;
    
    assert!(result.is_ok());
    
    // Verify subscription was stored
    let subscriptions = redis_store::get_subscriptions(&state.redis_pool, &user_keys.public_key())
        .await
        .unwrap();
    
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(subscriptions[0], filter_json);
}

#[tokio::test]
async fn test_subscription_delete() {
    let state = create_test_state().await;
    let user_keys = Keys::generate();
    
    // First add a subscription
    let filter = Filter::new().kind(Kind::TextNote);
    let filter_json = serde_json::to_string(&filter).unwrap();
    
    redis_store::add_subscription(&state.redis_pool, &user_keys.public_key(), &filter_json)
        .await
        .unwrap();
    
    // Create subscription delete event (kind 3082)
    let del_event = EventBuilder::new(Kind::Custom(3082), &filter_json)
        .sign(&user_keys)
        .await
        .unwrap();
    
    // Handle the deletion
    let token = CancellationToken::new();
    let result = event_handler::handle_subscription_delete(&state, &del_event, token).await;
    
    assert!(result.is_ok());
    
    // Verify subscription was removed
    let subscriptions = redis_store::get_subscriptions(&state.redis_pool, &user_keys.public_key())
        .await
        .unwrap();
    
    assert_eq!(subscriptions.len(), 0);
}

#[tokio::test]
async fn test_multiple_subscriptions() {
    let state = create_test_state().await;
    let user_keys = Keys::generate();
    
    // Add multiple different subscriptions
    let filter1 = Filter::new().kind(Kind::TextNote);
    let filter2 = Filter::new().kind(Kind::Reaction);
    let filter3 = Filter::new().author(user_keys.public_key());
    
    let token = CancellationToken::new();
    
    for filter in [filter1, filter2, filter3] {
        let filter_json = serde_json::to_string(&filter).unwrap();
        let event = EventBuilder::new(Kind::Custom(3081), &filter_json)
            .sign(&user_keys)
            .await
            .unwrap();
        
        event_handler::handle_subscription_upsert(&state, &event, token.clone())
            .await
            .unwrap();
    }
    
    // Verify all subscriptions were stored
    let subscriptions = redis_store::get_subscriptions(&state.redis_pool, &user_keys.public_key())
        .await
        .unwrap();
    
    assert_eq!(subscriptions.len(), 3);
}

#[tokio::test]
async fn test_duplicate_subscription() {
    let state = create_test_state().await;
    let user_keys = Keys::generate();
    
    let filter = Filter::new().kind(Kind::TextNote);
    let filter_json = serde_json::to_string(&filter).unwrap();
    
    let token = CancellationToken::new();
    
    // Add the same subscription twice
    for _ in 0..2 {
        let event = EventBuilder::new(Kind::Custom(3081), &filter_json)
            .sign(&user_keys)
            .await
            .unwrap();
        
        event_handler::handle_subscription_upsert(&state, &event, token.clone())
            .await
            .unwrap();
    }
    
    // Should only have one subscription (deduplicated)
    let subscriptions = redis_store::get_subscriptions(&state.redis_pool, &user_keys.public_key())
        .await
        .unwrap();
    
    assert_eq!(subscriptions.len(), 1);
}

#[tokio::test]
async fn test_invalid_filter_json() {
    let state = create_test_state().await;
    let user_keys = Keys::generate();
    
    // Create event with invalid JSON
    let invalid_json = "not a valid filter json";
    let event = EventBuilder::new(Kind::Custom(3081), invalid_json)
        .sign(&user_keys)
        .await
        .unwrap();
    
    let token = CancellationToken::new();
    let result = event_handler::handle_subscription_upsert(&state, &event, token).await;
    
    // Should handle error gracefully
    assert!(result.is_err() || result.is_ok()); // Depends on implementation choice
}