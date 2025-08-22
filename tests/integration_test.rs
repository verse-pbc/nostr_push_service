// Integration tests for plur_push_service

use anyhow::{anyhow, Result};
// Removed serial_test - tests now run in parallel with isolated Redis databases

mod common;
use bb8_redis::bb8; // Import bb8
use nostr_relay_builder::MockRelay;
use nostr_sdk::{
    ClientBuilder,
    Event,
    EventBuilder,
    Keys,
    Kind,
    PublicKey,
    Tag,
    ToBech32, // Import ToBech32 trait
};
use nostr_push_service::{
    config::Settings,
    redis_store::{self},
    state::AppState,
};
use redis::AsyncCommands; // For direct Redis interaction
use std::collections::HashSet; // Added for cache manipulation
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken; // For service shutdown
use url::Url;

// Import service components and event type
use nostr_push_service::{event_handler, nostr_listener};
use nostr_push_service::event_handler::EventContext;
use tokio::sync::mpsc; // Keep specific mpsc import for clarity

// --- Test Relay Setup ---

async fn setup_test_environment() -> Result<(
    Arc<AppState>,
    Arc<nostr_push_service::fcm_sender::MockFcmSender>,
    Url,
    MockRelay,
)> {
    dotenvy::dotenv().ok();
    
    // Use standard Redis URL (no database selection)
    let redis_url_constructed = common::create_test_redis_url();

    // --- Safety Check: Prevent running tests against DigitalOcean Redis ---
    if let Ok(parsed_url) = url::Url::parse(&redis_url_constructed) {
        if parsed_url
            .host_str()
            .is_some_and(|host| host.ends_with(".db.ondigitalocean.com"))
        {
            panic!(
                "Safety check failed: Redis URL points to a DigitalOcean managed database ({}). \
        Aborting test to prevent potential data loss.",
                redis_url_constructed
            );
        }
    }
    // --- End Safety Check ---

    // For testing purposes, we'll use a hardcoded test key in hex format
    let test_key_hex = "0000000000000000000000000000000000000000000000000000000000000001";

    // Set the environment variable with our test key hex
    std::env::set_var("NOSTR_PUSH__SERVICE__PRIVATE_KEY_HEX", test_key_hex);

    let mut settings = Settings::new()
        .map_err(|e| anyhow!("Failed to load settings after setting env var: {}", e))?;
    let test_service_keys = Keys::generate(); // Generate separate keys for the service instance

    let mock_relay = MockRelay::run().await?;
    let relay_url_str = mock_relay.url();

    let relay_url = Url::parse(&relay_url_str)?;

    settings.nostr.relay_url = relay_url_str.clone();

    // Add a longer delay to allow the Redis service container to fully initialize in CI
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Manually construct AppState to inject MockFcmSender
    let redis_pool = nostr_push_service::redis_store::create_pool(
        &redis_url_constructed, // Use the constructed URL
        settings.redis.connection_pool_size,
    )
    .await
    .map_err(|e| {
        println!("Redis pool creation error: {}", e);
        e
    })?;

    // Optional: Clean Redis once at the start of all tests
    // common::clean_redis_once(&redis_pool).await?;

    let mock_fcm_sender_instance = nostr_push_service::fcm_sender::MockFcmSender::new();
    let mock_fcm_sender_arc = Arc::new(mock_fcm_sender_instance.clone()); // Arc for returning

    // Create FcmClient wrapper with the mock implementation
    let fcm_client = Arc::new(nostr_push_service::fcm_sender::FcmClient::new_with_impl(
        Box::new(mock_fcm_sender_instance),
    ));

    // Initialize Nip29Client for the AppState, ensuring it connects to the mock relay
    let nip29_cache_expiration = settings.nostr.cache_expiration.unwrap_or(300);
    let nip29_client = nostr_push_service::nostr::nip29::Nip29Client::new(
        relay_url_str.clone(),     // Use the mock relay URL
        test_service_keys.clone(), // Use the service keys generated for this test instance
        nip29_cache_expiration,
    )
    .await
    .map_err(|e| anyhow!("Failed to create and connect Nip29Client: {}", e))?;

    // Add a small delay to ensure the client has time to establish the connection
    tokio::time::sleep(Duration::from_secs(1)).await;

    let app_state = nostr_push_service::state::AppState {
        settings,
        redis_pool,
        fcm_client,
        service_keys: Some(test_service_keys),
        nip29_client: Arc::new(nip29_client), // Add initialized Nip29Client
    };

    Ok((
        Arc::new(app_state),
        mock_fcm_sender_arc,
        relay_url,
        mock_relay,
    ))
}


#[tokio::test]
async fn test_register_device_token() -> Result<()> {
    let (state, _fcm_mock, _relay_url, _mock_relay) = setup_test_environment().await?;

    // Use unique test data for isolation
    let pubkey_hex = common::generate_test_pubkey_hex();
    let pubkey = PublicKey::from_hex(&pubkey_hex)?;
    let token = common::generate_test_token("registration");

    nostr_push_service::redis_store::add_or_update_token(&state.redis_pool, &pubkey, &token).await?;

    let stored_tokens =
        nostr_push_service::redis_store::get_tokens_for_pubkey(&state.redis_pool, &pubkey).await?;
    assert_eq!(stored_tokens.len(), 1);
    assert_eq!(stored_tokens[0], token);

    let mut conn =
        state
            .redis_pool
            .get()
            .await
            .map_err(|e: bb8::RunError<redis::RedisError>| {
                anyhow!("Failed to get Redis connection: {}", e)
            })?;
    let stored_pubkey_hex: Option<String> = conn
        .hget(nostr_push_service::redis_store::TOKEN_TO_PUBKEY_HASH, token)
        .await
        .map_err(|e: redis::RedisError| anyhow!("Failed to HGET token mapping: {}", e))?;
    assert_eq!(stored_pubkey_hex, Some(pubkey_hex.to_string()));

    Ok(())
}

#[tokio::test]
async fn test_event_handling() -> Result<()> {
    let (state, fcm_mock, relay_url, _mock_relay) = setup_test_environment().await?;

    let service_token = CancellationToken::new();
    let (event_tx, event_rx) = mpsc::channel::<(Box<Event>, EventContext)>(100);

    let listener_state = state.clone();
    let listener_token = service_token.clone();
    let listener_handle = tokio::spawn(async move {
        if let Err(e) = nostr_listener::run(listener_state, event_tx, listener_token).await {
            eprintln!("Nostr listener task error: {}", e);
        }
    });

    let handler_state = state.clone();
    let handler_token = service_token.clone();
    let handler_handle = tokio::spawn(async move {
        if let Err(e) = event_handler::run(handler_state, event_rx, handler_token).await {
            eprintln!("Event handler task error: {}", e);
        }
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    let user_a_keys = Keys::generate(); // User A is the one registering the token
    let user_a_client = ClientBuilder::new().signer(user_a_keys.clone()).build();
    user_a_client.add_relay(relay_url.as_str()).await?;
    user_a_client.connect().await;

    // --- Test Registration (User A) ---
    let fcm_token_user_a = "test_fcm_token_for_user_a";
    let registration_event = EventBuilder::new(Kind::Custom(3079), fcm_token_user_a)
        .sign(&user_a_keys)
        .await?;
    user_a_client.send_event(&registration_event).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let stored_tokens =
        redis_store::get_tokens_for_pubkey(&state.redis_pool, &user_a_keys.public_key()).await?;
    assert_eq!(stored_tokens.len(), 1);
    assert_eq!(stored_tokens[0], fcm_token_user_a);

    // --- Test Notification Send (User B sends message tagging User A in a group) ---
    let user_b_keys = Keys::generate(); // User B sends the message
    let user_b_client = ClientBuilder::new().signer(user_b_keys.clone()).build();
    user_b_client.add_relay(relay_url.as_str()).await?;
    user_b_client.connect().await;

    // Define group and add User A to Nip29 cache manually
    let test_group_id = "test_group_integration";
    {
        let cache_arc = (*state.nip29_client).get_cache();
        let mut cache = cache_arc.write().await;
        let mut members = HashSet::new();
        members.insert(user_a_keys.public_key());
        cache.update_cache(test_group_id, members);
    }

    let mention_tag = Tag::public_key(user_a_keys.public_key());
    let group_tag = Tag::parse(["h", test_group_id])?;
    let message_content = format!("Hello @{}", user_a_keys.public_key().to_bech32()?);

    let message_event_builder = EventBuilder::new(Kind::Custom(11), &message_content);
    let message_event = message_event_builder
        .tag(mention_tag)
        .tag(group_tag)
        .sign(&user_b_keys)
        .await?;

    user_a_client.send_event(&message_event).await?;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Assert FCM mock received the notification for User A's token
    let sent_fcm_messages = fcm_mock.get_sent_messages();
    assert_eq!(
        sent_fcm_messages.len(),
        1,
        "Expected 1 FCM message to be sent"
    );
    let (sent_token, sent_payload) = &sent_fcm_messages[0];
    assert_eq!(
        sent_token, fcm_token_user_a,
        "FCM message sent to wrong token"
    );

    // Basic payload check - now expects data-only message (no notification field)
    assert!(
        sent_payload.notification.is_none(),
        "FCM payload should not have notification field (data-only message)"
    );
    assert!(sent_payload.data.is_some(), "FCM payload data missing");
    let data = sent_payload.data.as_ref().unwrap();
    assert!(
        data.contains_key("title"),
        "FCM data should contain title"
    );
    assert!(
        data.contains_key("body"),
        "FCM data should contain body"
    );
    assert_eq!(
        data.get("nostrEventId").map(|s| s.as_str()),
        Some(message_event.id.to_hex().as_str())
    );

    // TODO: 8. Publish Kind 3080 (Deregistration) event
    // ... (event creation, publishing) ...
    let deregistration_event = EventBuilder::new(Kind::Custom(3080), fcm_token_user_a)
        .sign(&user_a_keys)
        .await?;
    user_a_client.send_event(&deregistration_event).await?;

    // TODO: 9. Assert Redis state for deregistration
    // ... (redis checks) ...
    tokio::time::sleep(Duration::from_millis(500)).await; // Give time for event to be processed

    let stored_tokens_after_deregistration =
        redis_store::get_tokens_for_pubkey(&state.redis_pool, &user_a_keys.public_key()).await?;
    assert!(
        stored_tokens_after_deregistration.is_empty(),
        "Expected no tokens for user A after deregistration, found: {:?}",
        stored_tokens_after_deregistration
    );

    // --- Test Notification NOT Sent After Deregistration ---
    let message_content_after_dereg = format!(
        "Hello again @{} after deregistration",
        user_a_keys.public_key().to_bech32()?
    );
    let message_event_after_dereg_builder =
        EventBuilder::new(Kind::Custom(11), &message_content_after_dereg);
    let message_event_after_dereg = message_event_after_dereg_builder
        .tag(Tag::public_key(user_a_keys.public_key())) // Re-tag User A
        .sign(&user_b_keys)
        .await?;

    user_b_client.send_event(&message_event_after_dereg).await?;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let sent_fcm_messages_after_dereg = fcm_mock.get_sent_messages();
    assert_eq!(
        sent_fcm_messages_after_dereg.len(),
        1, // Should still be 1 from the *first* notification
        "Expected FCM message count to remain 1 after deregistration, but it changed."
    );

    // --- Shutdown ---
    user_a_client.disconnect().await;
    user_b_client.disconnect().await;
    service_token.cancel();
    let _ = listener_handle.await;
    let _ = handler_handle.await;
    Ok(())
}
