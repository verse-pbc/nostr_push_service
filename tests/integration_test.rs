// Integration tests for plur_push_service

use anyhow::{anyhow, Result};
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
use plur_push_service::{
    config::Settings,
    redis_store::{self, RedisPool},
    state::AppState,
};
use redis::AsyncCommands; // For direct Redis interaction
use std::env;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken; // For service shutdown
use url::Url;

// Import service components and event type
use plur_push_service::{event_handler, nostr_listener};
use tokio::sync::mpsc; // Keep specific mpsc import for clarity

// --- Test Relay Setup ---

async fn setup_test_environment() -> Result<(
    Arc<AppState>,
    Arc<plur_push_service::fcm_sender::MockFcmSender>,
    Url,
    MockRelay,
)> {
    dotenvy::dotenv().ok();
    let redis_url_env = env::var("REDIS_URL").expect("REDIS_URL must be set for integration tests");

    // --- Safety Check: Prevent running tests against DigitalOcean Redis ---
    if let Ok(parsed_url) = url::Url::parse(&redis_url_env) {
        if parsed_url
            .host_str()
            .map_or(false, |host| host.ends_with(".db.ondigitalocean.com"))
        {
            panic!(
                "Safety check failed: REDIS_URL points to a DigitalOcean managed database ({}). \
        Aborting test to prevent potential data loss.",
                redis_url_env
            );
        }
    }
    // --- End Safety Check ---

    let mut settings = Settings::new()?;

    println!("Starting MockRelay...");
    let mock_relay = MockRelay::run().await?;
    let relay_url_str = mock_relay.url();
    println!("MockRelay running at: {}", relay_url_str);

    let relay_url = Url::parse(&relay_url_str)?;

    settings.nostr.relay_url = relay_url_str;

    // Manually construct AppState to inject MockFcmSender
    let redis_pool = plur_push_service::redis_store::create_pool(
        &redis_url_env, // Use the env var directly, as AppState::new() did
        settings.redis.connection_pool_size,
    )
    .await?;
    cleanup_redis(&redis_pool).await?; // Cleanup *before* returning state

    let mock_fcm_sender_instance = plur_push_service::fcm_sender::MockFcmSender::new();
    let mock_fcm_sender_arc = Arc::new(mock_fcm_sender_instance.clone()); // Arc for returning

    // Create FcmClient wrapper with the mock implementation
    let fcm_client = Arc::new(plur_push_service::fcm_sender::FcmClient::new_with_impl(
        Box::new(mock_fcm_sender_instance),
    ));

    let service_keys = settings.get_service_keys();

    let app_state = plur_push_service::state::AppState {
        settings,
        redis_pool,
        fcm_client,
        service_keys,
    };

    Ok((
        Arc::new(app_state),
        mock_fcm_sender_arc,
        relay_url,
        mock_relay,
    ))
}

async fn cleanup_redis(pool: &RedisPool) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e: bb8::RunError<redis::RedisError>| {
            anyhow!("Failed to get Redis connection: {}", e)
        })?;
    redis::cmd("FLUSHDB")
        .query_async::<()>(&mut *conn)
        .await
        .map_err(|e: redis::RedisError| anyhow!("Failed to flush Redis DB: {}", e))?;
    Ok(())
}

#[tokio::test]
async fn test_register_device_token() -> Result<()> {
    let (state, _fcm_mock, _relay_url, _mock_relay) = setup_test_environment().await?;

    let pubkey_hex = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    let pubkey = PublicKey::from_hex(pubkey_hex)?;
    let token = "test_token_for_registration";

    plur_push_service::redis_store::add_or_update_token(&state.redis_pool, &pubkey, token).await?;

    let stored_tokens =
        plur_push_service::redis_store::get_tokens_for_pubkey(&state.redis_pool, &pubkey).await?;
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
        .hget(plur_push_service::redis_store::TOKEN_TO_PUBKEY_HASH, token)
        .await
        .map_err(|e: redis::RedisError| anyhow!("Failed to HGET token mapping: {}", e))?;
    assert_eq!(stored_pubkey_hex, Some(pubkey_hex.to_string()));

    Ok(())
}

#[tokio::test]
async fn test_event_handling() -> Result<()> {
    println!("Setting up test environment...");
    let (state, fcm_mock, relay_url, _mock_relay) = setup_test_environment().await?;
    println!("Test environment ready.");

    let service_token = CancellationToken::new();
    let (event_tx, event_rx) = mpsc::channel::<Box<Event>>(100);

    let listener_state = state.clone();
    let listener_token = service_token.clone();
    let listener_handle = tokio::spawn(async move {
        println!("Starting Nostr listener task...");
        if let Err(e) = nostr_listener::run(listener_state, event_tx, listener_token).await {
            eprintln!("Nostr listener task error: {}", e);
        }
        println!("Nostr listener task finished.");
    });

    let handler_state = state.clone();
    let handler_token = service_token.clone();
    let handler_handle = tokio::spawn(async move {
        println!("Starting event handler task...");
        if let Err(e) = event_handler::run(handler_state, event_rx, handler_token).await {
            eprintln!("Event handler task error: {}", e);
        }
        println!("Event handler task finished.");
    });

    println!("Waiting for services to initialize...");
    tokio::time::sleep(Duration::from_millis(500)).await;
    println!("Services potentially initialized.");

    println!("Connecting test Nostr client (user A)...");
    let user_a_keys = Keys::generate(); // User A is the one registering the token
    let user_a_client = ClientBuilder::new().signer(user_a_keys.clone()).build();
    user_a_client.add_relay(relay_url.as_str()).await?;
    user_a_client.connect().await;
    println!("User A client connected to {}", relay_url);

    // --- Test Registration (User A) ---
    println!("Testing registration (Kind 3079) for User A...");
    let fcm_token_user_a = "test_fcm_token_for_user_a";
    let registration_event = EventBuilder::new(Kind::Custom(3079), fcm_token_user_a)
        .sign(&user_a_keys)
        .await?;
    println!("Sending registration event ID: {}", registration_event.id);
    user_a_client.send_event(&registration_event).await?;
    println!("Waiting for registration event processing...");
    tokio::time::sleep(Duration::from_millis(500)).await;
    println!("Asserting Redis state after registration...");
    let stored_tokens =
        redis_store::get_tokens_for_pubkey(&state.redis_pool, &user_a_keys.public_key()).await?;
    assert_eq!(stored_tokens.len(), 1);
    assert_eq!(stored_tokens[0], fcm_token_user_a);
    println!("Registration Redis state verified.");

    // --- Test Notification Send (User B sends message tagging User A) ---
    println!("Connecting User B client...");
    let user_b_keys = Keys::generate(); // User B sends the message
    let user_b_client = ClientBuilder::new().signer(user_b_keys.clone()).build();
    user_b_client.add_relay(relay_url.as_str()).await?;
    user_b_client.connect().await;
    println!("User B client connected.");

    println!("Testing notification send (Kind 11) from User B to User A...");
    let mention_tag = Tag::public_key(user_a_keys.public_key());
    let message_content = format!("Hello @{}", user_a_keys.public_key().to_bech32()?);

    // Try adding tag after EventBuilder::new
    let message_event_builder = EventBuilder::new(Kind::Custom(11), &message_content);
    let message_event = message_event_builder
        .tag(mention_tag)
        .sign(&user_b_keys)
        .await?;

    println!("Sending Kind 11 event ID: {}", message_event.id);
    user_b_client.send_event(&message_event).await?;
    println!("Waiting for notification event processing...");
    tokio::time::sleep(Duration::from_millis(500)).await; // Adjust timing if needed

    // Assert FCM mock received the notification for User A's token
    println!("Asserting FCM mock state...");
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

    // Basic payload check (adapt based on create_fcm_payload logic)
    assert!(
        sent_payload.notification.is_some(),
        "FCM payload notification missing"
    );
    let notification = sent_payload.notification.as_ref().unwrap();
    assert!(
        notification.title.is_some(),
        "FCM notification title missing"
    );
    // Could add checks for title content, body content, data fields etc.
    // Example: Check if event ID is in data
    assert!(sent_payload.data.is_some(), "FCM payload data missing");
    let data = sent_payload.data.as_ref().unwrap();
    assert_eq!(
        data.get("nostrEventId").map(|s| s.as_str()),
        Some(message_event.id.to_hex().as_str())
    );
    println!("FCM mock state verified.");

    // TODO: 8. Publish Kind 3080 (Deregistration) event
    // ... (event creation, publishing) ...
    println!("Testing deregistration (Kind 3080) for User A...");
    let deregistration_event = EventBuilder::new(Kind::Custom(3080), fcm_token_user_a)
        .sign(&user_a_keys)
        .await?;
    println!(
        "Sending deregistration event ID: {}",
        deregistration_event.id
    );
    user_a_client.send_event(&deregistration_event).await?;

    // TODO: 9. Assert Redis state for deregistration
    // ... (redis checks) ...
    println!("Waiting for deregistration event processing...");
    tokio::time::sleep(Duration::from_millis(500)).await; // Give time for event to be processed

    println!("Asserting Redis state after deregistration...");
    let stored_tokens_after_deregistration =
        redis_store::get_tokens_for_pubkey(&state.redis_pool, &user_a_keys.public_key()).await?;
    assert!(
        stored_tokens_after_deregistration.is_empty(),
        "Expected no tokens for user A after deregistration, found: {:?}",
        stored_tokens_after_deregistration
    );
    println!("Deregistration Redis state verified.");

    // --- Test Notification NOT Sent After Deregistration ---
    println!("Testing notification NOT sent after deregistration...");
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

    println!(
        "Sending second Kind 11 event ID: {}",
        message_event_after_dereg.id
    );
    user_b_client.send_event(&message_event_after_dereg).await?;

    println!("Waiting for potential (unwanted) notification processing...");
    tokio::time::sleep(Duration::from_millis(500)).await;

    println!("Asserting FCM mock state unchanged after deregistration...");
    let sent_fcm_messages_after_dereg = fcm_mock.get_sent_messages();
    assert_eq!(
        sent_fcm_messages_after_dereg.len(),
        1, // Should still be 1 from the *first* notification
        "Expected FCM message count to remain 1 after deregistration, but it changed."
    );
    println!("FCM mock state verified (no new message sent).");

    // --- Shutdown ---
    println!("Initiating shutdown sequence...");
    println!("Disconnecting User A client...");
    user_a_client.disconnect().await;
    println!("User A client disconnected.");
    println!("Disconnecting User B client...");
    user_b_client.disconnect().await;
    println!("User B client disconnected.");
    // ... rest of shutdown ...
    println!("Cancelling service tasks...");
    service_token.cancel();
    println!("Waiting for listener task to complete...");
    let _ = listener_handle.await;
    println!("Waiting for handler task to complete...");
    let _ = handler_handle.await;
    println!("Test finished.");
    Ok(())
}
