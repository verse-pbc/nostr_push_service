use anyhow::{anyhow, Result};
use nostr_relay_builder::MockRelay;
use serial_test::serial;
use nostr_sdk::{ClientBuilder, Event, EventBuilder, Keys, Kind, PublicKey, Tag, ToBech32};
use plur_push_service::{
    config::Settings,
    redis_store::{self, RedisPool},
    state::AppState,
};
use std::collections::HashSet;
use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::Url;

// Global counter for unique test IDs
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

// Import service components
use plur_push_service::{event_handler, nostr_listener};
use plur_push_service::event_handler::EventContext;
use tokio::sync::mpsc;

/// Test environment setup, similar to integration_test.rs but focused on broadcast tests
async fn setup_broadcast_test_environment() -> Result<(
    Arc<AppState>,
    Arc<plur_push_service::fcm_sender::MockFcmSender>,
    Url,
    MockRelay,
)> {
    dotenvy::dotenv().ok();
    // Use REDIS_HOST and REDIS_PORT, falling back to defaults if not set
    let redis_host = env::var("REDIS_HOST").unwrap_or_else(|_| "localhost".to_string());
    let redis_port = env::var("REDIS_PORT").unwrap_or_else(|_| "6379".to_string());
    let redis_url_constructed = format!("redis://{}:{}", redis_host, redis_port);

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

    // For testing purposes, use a hardcoded test key in hex format
    let test_key_hex = "0000000000000000000000000000000000000000000000000000000000000001";
    std::env::set_var("PLUR_PUSH__SERVICE__PRIVATE_KEY_HEX", test_key_hex);

    let mut settings = Settings::new()
        .map_err(|e| anyhow!("Failed to load settings after setting env var: {}", e))?;
    let test_service_keys = Keys::generate();

    let mock_relay = MockRelay::run().await?;
    let relay_url_str = mock_relay.url();
    let relay_url = Url::parse(&relay_url_str)?;

    settings.nostr.relay_url = relay_url_str.clone();

    // Allow time for Redis to initialize
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Create Redis pool
    let redis_pool = plur_push_service::redis_store::create_pool(
        &redis_url_constructed,
        settings.redis.connection_pool_size,
    )
    .await
    .map_err(|e| {
        println!("Redis pool creation error: {}", e);
        e
    })?;

    // Clean up Redis before test
    println!("Cleaning up Redis...");
    cleanup_redis(&redis_pool).await?;
    println!("Redis cleanup completed successfully.");

    // Create mock FCM sender
    let mock_fcm_sender_instance = plur_push_service::fcm_sender::MockFcmSender::new();
    let mock_fcm_sender_arc = Arc::new(mock_fcm_sender_instance.clone());

    // Create FcmClient wrapper with the mock implementation
    let fcm_client = Arc::new(plur_push_service::fcm_sender::FcmClient::new_with_impl(
        Box::new(mock_fcm_sender_instance),
    ));

    // Initialize Nip29Client for the AppState
    let nip29_cache_expiration = settings.nostr.cache_expiration.unwrap_or(300);
    let nip29_client = plur_push_service::nostr::nip29::Nip29Client::new(
        relay_url_str.clone(),
        test_service_keys.clone(),
        nip29_cache_expiration,
    )
    .await
    .map_err(|e| anyhow!("Failed to create and connect Nip29Client: {}", e))?;

    // Add a small delay to ensure the client has time to establish the connection
    tokio::time::sleep(Duration::from_secs(1)).await;

    let app_state = plur_push_service::state::AppState {
        settings,
        redis_pool,
        fcm_client,
        service_keys: Some(test_service_keys),
        nip29_client: Arc::new(nip29_client),
    };

    Ok((
        Arc::new(app_state),
        mock_fcm_sender_arc,
        relay_url,
        mock_relay,
    ))
}

async fn cleanup_redis(pool: &RedisPool) -> Result<()> {
    // Flush Redis DB for test isolation (tests run serially)
    let mut conn = pool.get().await?;
    redis::cmd("FLUSHDB")
        .query_async::<()>(&mut *conn)
        .await?;
    Ok(())
}

/// Helper to register a user's FCM token
async fn register_user_token(
    state: &Arc<AppState>,
    user_keys: &Keys,
    client: &nostr_sdk::Client,
    token: &str,
) -> Result<()> {
    let registration_event = EventBuilder::new(Kind::Custom(3079), token)
        .sign(user_keys)
        .await?;
    client.send_event(&registration_event).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify token was registered
    let stored_tokens =
        redis_store::get_tokens_for_pubkey(&state.redis_pool, &user_keys.public_key()).await?;
    assert_eq!(stored_tokens.len(), 1);
    assert_eq!(stored_tokens[0], token);

    Ok(())
}

/// Helper to set up a test group with members AND admins published to relay
async fn setup_test_group_with_admin(
    state: &Arc<AppState>,
    group_id: &str,
    admin_pubkeys: HashSet<PublicKey>,
    member_pubkeys: HashSet<PublicKey>,
) -> Result<()> {
    let service_keys = state
        .service_keys
        .as_ref()
        .ok_or_else(|| anyhow!("Service keys missing from AppState"))?;
    let nip29_nostr_client = state.nip29_client.client();

    // --- Publish Admins (Kind 39001) ---
    let mut admin_tags: Vec<Tag> = admin_pubkeys
        .into_iter()
        .map(Tag::public_key)
        .collect();
    admin_tags.push(Tag::identifier(group_id.to_string()));

    let admin_event = EventBuilder::new(Kind::Custom(39001), "")
        .tags(admin_tags)
        .sign(service_keys)
        .await?;
    nip29_nostr_client.send_event(&admin_event).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // --- Publish Members (Kind 39002) ---
    let mut member_tags: Vec<Tag> = member_pubkeys
        .into_iter()
        .map(Tag::public_key)
        .collect();
    member_tags.push(Tag::identifier(group_id.to_string()));

    let member_event = EventBuilder::new(Kind::Custom(39002), "")
        .tags(member_tags)
        .sign(service_keys)
        .await?;
    nip29_nostr_client.send_event(&member_event).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    Ok(())
}

/// Test 1: Verify broadcast messages send notifications to all group members
#[tokio::test]
#[serial]
async fn test_broadcast_notifications() -> Result<()> {
    let (state, fcm_mock, relay_url, _mock_relay) = setup_broadcast_test_environment().await?;

    // Set up service with event handler and listener
    let service_token = CancellationToken::new();
    let (event_tx, event_rx) = mpsc::channel::<(Box<Event>, EventContext)>(100);

    // Start nostr listener
    let listener_state = state.clone();
    let listener_token = service_token.clone();
    let listener_handle = tokio::spawn(async move {
        if let Err(e) = nostr_listener::run(listener_state, event_tx, listener_token).await {
            eprintln!("Nostr listener task error: {}", e);
        }
    });

    // Start event handler
    let handler_state = state.clone();
    let handler_token = service_token.clone();
    let handler_handle = tokio::spawn(async move {
        if let Err(e) = event_handler::run(handler_state, event_rx, handler_token).await {
            eprintln!("Event handler task error: {}", e);
        }
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Create test group members
    let group_id = "test_broadcast_group";
    let sender_keys = Keys::generate();
    let sender_client = ClientBuilder::new().signer(sender_keys.clone()).build();
    sender_client.add_relay(relay_url.as_str()).await?;
    sender_client.connect().await;

    // Create 3 receiver users
    let user1_keys = Keys::generate();
    let user1_client = ClientBuilder::new().signer(user1_keys.clone()).build();
    user1_client.add_relay(relay_url.as_str()).await?;
    user1_client.connect().await;

    let user2_keys = Keys::generate();
    let user2_client = ClientBuilder::new().signer(user2_keys.clone()).build();
    user2_client.add_relay(relay_url.as_str()).await?;
    user2_client.connect().await;

    let user3_keys = Keys::generate();
    let user3_client = ClientBuilder::new().signer(user3_keys.clone()).build();
    user3_client.add_relay(relay_url.as_str()).await?;
    user3_client.connect().await;

    // Register FCM tokens for each user
    let token1 = "fcm_token_user1_broadcast";
    let token2 = "fcm_token_user2_broadcast";
    let token3 = "fcm_token_user3_broadcast";

    register_user_token(&state, &user1_keys, &user1_client, &token1).await?;
    register_user_token(&state, &user2_keys, &user2_client, &token2).await?;
    register_user_token(&state, &user3_keys, &user3_client, &token3).await?;

    // Define admins and members
    let mut admins = HashSet::new();
    admins.insert(sender_keys.public_key());

    let mut members = HashSet::new();
    members.insert(sender_keys.public_key());
    members.insert(user1_keys.public_key());
    members.insert(user2_keys.public_key());
    members.insert(user3_keys.public_key());

    // Set up test group using AppState for service keys/client
    setup_test_group_with_admin(&state, group_id, admins.clone(), members.clone()).await?;

    // Send a broadcast message
    let group_tag = Tag::parse(["h", &group_id])?;
    let broadcast_tag = Tag::parse(["broadcast"])?;
    let message_content = "This is a broadcast message to the group";

    let broadcast_event = EventBuilder::new(Kind::Custom(9), message_content)
        .tag(group_tag)
        .tag(broadcast_tag)
        .sign(&sender_keys)
        .await?;

    sender_client.send_event(&broadcast_event).await?;

    // Wait for events to be processed
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Check that FCM notifications were sent to all users except sender
    let sent_fcm_messages = fcm_mock.get_sent_messages();

    // Should have 3 notifications (one for each user except sender)
    assert_eq!(
        sent_fcm_messages.len(),
        3,
        "Expected 3 FCM messages for broadcast, got {}",
        sent_fcm_messages.len()
    );

    // Verify tokens used for sending are correct (all users should get notified)
    let sent_tokens: Vec<&String> = sent_fcm_messages.iter().map(|(token, _)| token).collect();
    assert!(
        sent_tokens.contains(&&token1.to_string()),
        "Expected user1 token in FCM messages"
    );
    assert!(
        sent_tokens.contains(&&token2.to_string()),
        "Expected user2 token in FCM messages"
    );
    assert!(
        sent_tokens.contains(&&token3.to_string()),
        "Expected user3 token in FCM messages"
    );

    // Clean up
    sender_client.disconnect().await;
    user1_client.disconnect().await;
    user2_client.disconnect().await;
    user3_client.disconnect().await;
    service_token.cancel();
    let _ = listener_handle.await;
    let _ = handler_handle.await;

    Ok(())
}

/// Test 2: Verify regular mention-based notifications still work alongside broadcast
#[tokio::test]
#[serial]
async fn test_regular_mentions_with_broadcast() -> Result<()> {
    let (state, fcm_mock, relay_url, _mock_relay) = setup_broadcast_test_environment().await?;

    // Set up service
    let service_token = CancellationToken::new();
    let (event_tx, event_rx) = mpsc::channel::<(Box<Event>, EventContext)>(100);

    let listener_state = state.clone();
    let listener_token = service_token.clone();
    let _listener_handle = tokio::spawn(async move {
        if let Err(e) = nostr_listener::run(listener_state, event_tx, listener_token).await {
            eprintln!("Nostr listener task error: {}", e);
        }
    });

    let handler_state = state.clone();
    let handler_token = service_token.clone();
    let _handler_handle = tokio::spawn(async move {
        if let Err(e) = event_handler::run(handler_state, event_rx, handler_token).await {
            eprintln!("Event handler task error: {}", e);
        }
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Create test users and group
    let group_id = "test_regular_mention_group";
    let sender_keys = Keys::generate();
    let sender_client = ClientBuilder::new().signer(sender_keys.clone()).build();
    sender_client.add_relay(relay_url.as_str()).await?;
    sender_client.connect().await;

    // Create 3 receiver users
    let user1_keys = Keys::generate();
    let user1_client = ClientBuilder::new().signer(user1_keys.clone()).build();
    user1_client.add_relay(relay_url.as_str()).await?;
    user1_client.connect().await;

    let user2_keys = Keys::generate();
    let user2_client = ClientBuilder::new().signer(user2_keys.clone()).build();
    user2_client.add_relay(relay_url.as_str()).await?;
    user2_client.connect().await;

    let user3_keys = Keys::generate();
    let user3_client = ClientBuilder::new().signer(user3_keys.clone()).build();
    user3_client.add_relay(relay_url.as_str()).await?;
    user3_client.connect().await;

    // Register FCM tokens
    let token1 = "fcm_token_user1_mention";
    let token2 = "fcm_token_user2_mention";
    let token3 = "fcm_token_user3_mention";

    register_user_token(&state, &user1_keys, &user1_client, &token1).await?;
    register_user_token(&state, &user2_keys, &user2_client, &token2).await?;
    register_user_token(&state, &user3_keys, &user3_client, &token3).await?;

    // Define admins and members
    let mut admins = HashSet::new();
    admins.insert(sender_keys.public_key());

    let mut members = HashSet::new();
    members.insert(sender_keys.public_key());
    members.insert(user1_keys.public_key());
    members.insert(user2_keys.public_key());
    members.insert(user3_keys.public_key());

    // Set up test group using AppState
    setup_test_group_with_admin(&state, group_id, admins.clone(), members.clone()).await?;

    // 1. Send a regular mention-based message (mentions user1 only)
    let group_tag = Tag::parse(["h", &group_id])?;
    let mention_tag = Tag::public_key(user1_keys.public_key());
    let message_content = format!("Hello @{}", user1_keys.public_key().to_bech32()?);

    let mention_event = EventBuilder::new(Kind::Custom(9), &message_content)
        .tag(group_tag.clone())
        .tag(mention_tag)
        .sign(&sender_keys)
        .await?;

    sender_client.send_event(&mention_event).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify only user1 received a notification
    let sent_fcm_messages = fcm_mock.get_sent_messages();
    assert_eq!(
        sent_fcm_messages.len(),
        1,
        "Expected 1 FCM message for mention notification"
    );
    assert_eq!(
        sent_fcm_messages[0].0,
        token1.to_string(),
        "Expected notification to be sent to user1"
    );

    // 2. Now test broadcasting in the same environment
    // 3. Send a message with both a mention and a broadcast tag
    let broadcast_tag = Tag::parse(["broadcast"])?;
    let broadcast_content = "Broadcast message to all group members";

    let broadcast_event = EventBuilder::new(Kind::Custom(9), broadcast_content)
        .tag(group_tag.clone())
        .tag(broadcast_tag)
        .sign(&sender_keys)
        .await?;

    // Send the broadcast event
    sender_client.send_event(&broadcast_event).await?;

    // Wait longer for processing
    tokio::time::sleep(Duration::from_secs(3)).await;

    // The broadcast should have triggered 3 more messages (to all users)
    // Total should now be 4 (1 from the mention + 3 from broadcast)
    let all_fcm_messages = fcm_mock.get_sent_messages();
    assert_eq!(
        all_fcm_messages.len(),
        4,
        "Expected 4 total FCM messages (1 from mention + 3 from broadcast)"
    );

    // Verify all users got the broadcast message
    let sent_tokens: Vec<&String> = all_fcm_messages.iter().map(|(token, _)| token).collect();

    // Each token should appear at least once
    assert!(
        sent_tokens.contains(&&token1.to_string()),
        "Expected user1 token in FCM messages"
    );
    assert!(
        sent_tokens.contains(&&token2.to_string()),
        "Expected user2 token in FCM messages"
    );
    assert!(
        sent_tokens.contains(&&token3.to_string()),
        "Expected user3 token in FCM messages"
    );

    // Clean up
    sender_client.disconnect().await;
    user1_client.disconnect().await;
    user2_client.disconnect().await;
    user3_client.disconnect().await;
    service_token.cancel();
    let _ = _listener_handle.await;
    let _ = _handler_handle.await;

    Ok(())
}

/// Test 3: Performance test with large groups
#[tokio::test]
#[serial]
async fn test_broadcast_performance_large_group() -> Result<()> {
    let (state, fcm_mock, relay_url, _mock_relay) = setup_broadcast_test_environment().await?;

    // Set up service
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

    // Create unique test ID for this test
    let test_id_base = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    
    // Create a large group with 50 members
    let large_group_id = format!("test_large_group_{}", test_id_base);
    let sender_keys = Keys::generate();
    let sender_client = ClientBuilder::new().signer(sender_keys.clone()).build();
    sender_client.add_relay(relay_url.as_str()).await?;
    sender_client.connect().await;

    // Create and register 50 members (10 with tokens, the rest without)
    let mut group_members = HashSet::new();
    group_members.insert(sender_keys.public_key());

    // Users with tokens
    let mut users_with_tokens = Vec::new();
    for i in 0..10 {
        let user_keys = Keys::generate();
        let user_client = ClientBuilder::new().signer(user_keys.clone()).build();
        user_client.add_relay(relay_url.as_str()).await?;
        user_client.connect().await;

        let token = format!("fcm_token_large_group_user{}_{}", i, test_id_base);
        register_user_token(&state, &user_keys, &user_client, &token).await?;

        group_members.insert(user_keys.public_key());
        users_with_tokens.push((user_keys, user_client));
    }

    // Users without tokens (just to increase group size)
    for _ in 0..40 {
        let user_keys = Keys::generate();
        group_members.insert(user_keys.public_key());
    }

    // Define admins and members
    let mut admins = HashSet::new();
    admins.insert(sender_keys.public_key());

    let mut members = HashSet::new();
    members.insert(sender_keys.public_key());
    for (user_keys, _) in &users_with_tokens {
        members.insert(user_keys.public_key());
    }

    // Set up large test group using AppState
    setup_test_group_with_admin(
        &state,
        &large_group_id,
        admins.clone(),
        group_members.clone(),
    )
    .await?;

    // Send a broadcast message to the large group
    let group_tag = Tag::parse(["h", &large_group_id])?;
    let broadcast_tag = Tag::parse(["broadcast"])?;
    let message_content = "This is a broadcast message to a large group";

    // Measure the time taken to process the broadcast
    let start_time = std::time::Instant::now();

    let broadcast_event = EventBuilder::new(Kind::Custom(9), message_content)
        .tag(group_tag)
        .tag(broadcast_tag)
        .sign(&sender_keys)
        .await?;

    sender_client.send_event(&broadcast_event).await?;

    // Wait a bit longer for large group processing
    tokio::time::sleep(Duration::from_secs(3)).await;

    let elapsed = start_time.elapsed();
    println!("Time to process large group broadcast: {:?}", elapsed);

    // Verify notifications were sent to all users with tokens
    let sent_fcm_messages = fcm_mock.get_sent_messages();
    assert_eq!(
        sent_fcm_messages.len(),
        10,
        "Expected 10 FCM messages for users with tokens in large group"
    );

    // Clean up
    sender_client.disconnect().await;
    for (_, client) in users_with_tokens {
        client.disconnect().await;
    }
    service_token.cancel();
    let _ = listener_handle.await;
    let _ = handler_handle.await;

    Ok(())
}

/// Test 4: Direct broadcast tag test to ensure proper tag detection
#[tokio::test]
#[serial]
async fn test_broadcast_tag_detection() -> Result<()> {
    // Create unique test ID for this test
    let test_id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    
    // Create a test event with a broadcast tag
    let keys = Keys::generate();
    let group_id = format!("test_tag_detection_group_{}", test_id);
    let group_tag = Tag::parse(["h", &group_id])?;
    let broadcast_tag = Tag::parse(["broadcast"])?;
    let message_content = "This is a test broadcast message";

    let test_event = EventBuilder::new(Kind::Custom(9), message_content)
        .tag(group_tag.clone())
        .tag(broadcast_tag)
        .sign(&keys)
        .await?;

    // Directly check if the event has a broadcast tag
    let is_broadcast = test_event
        .tags
        .find(nostr_sdk::TagKind::custom("broadcast"))
        .is_some();
    assert!(is_broadcast, "Failed to detect broadcast tag in event");

    // Test with malformed/differently-cased broadcast tag
    let malformed_tag = Tag::parse(["BrOaDcAsT"])?;
    let event_with_malformed_tag = EventBuilder::new(Kind::Custom(9), message_content)
        .tag(group_tag.clone())
        .tag(malformed_tag)
        .sign(&keys)
        .await?;

    let is_malformed_broadcast = event_with_malformed_tag
        .tags
        .find(nostr_sdk::TagKind::custom("broadcast"))
        .is_some();
    assert!(
        !is_malformed_broadcast,
        "Incorrectly detected malformed broadcast tag"
    );

    // Test with no broadcast tag
    let event_without_broadcast = EventBuilder::new(Kind::Custom(9), message_content)
        .tag(group_tag.clone())
        .sign(&keys)
        .await?;

    let has_broadcast_tag = event_without_broadcast
        .tags
        .find(nostr_sdk::TagKind::custom("broadcast"))
        .is_some();
    assert!(
        !has_broadcast_tag,
        "Incorrectly detected broadcast tag in event without one"
    );

    Ok(())
}

/// Test 5: Admin permission verification for broadcast messages
#[tokio::test]
#[serial]
async fn test_admin_permission_verification() -> Result<()> {
    let (state, fcm_mock, relay_url, _mock_relay) = setup_broadcast_test_environment().await?;

    // Set up service
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

    // Create test group
    let group_id = "test_admin_permission_group";
    
    // Admin user
    let admin_keys = Keys::generate();
    let admin_client = ClientBuilder::new().signer(admin_keys.clone()).build();
    admin_client.add_relay(relay_url.as_str()).await?;
    admin_client.connect().await;
    
    // Non-admin user
    let non_admin_keys = Keys::generate();
    let non_admin_client = ClientBuilder::new().signer(non_admin_keys.clone()).build();
    non_admin_client.add_relay(relay_url.as_str()).await?;
    non_admin_client.connect().await;
    
    // Regular member
    let member_keys = Keys::generate();
    let member_client = ClientBuilder::new().signer(member_keys.clone()).build();
    member_client.add_relay(relay_url.as_str()).await?;
    member_client.connect().await;

    // Register FCM token for member (to receive notifications)
    let member_token = "fcm_token_group_member";
    register_user_token(&state, &member_keys, &member_client, &member_token).await?;

    // Define admins and members
    let mut admins = HashSet::new();
    admins.insert(admin_keys.public_key()); // Only admin_keys is in the admin list
    
    let mut members = HashSet::new();
    members.insert(admin_keys.public_key());
    members.insert(non_admin_keys.public_key());
    members.insert(member_keys.public_key());

    // Set up test group
    setup_test_group_with_admin(&state, group_id, admins.clone(), members.clone()).await?;

    // 1. Test broadcast from admin user (should work)
    let group_tag = Tag::parse(["h", &group_id])?;
    let broadcast_tag = Tag::parse(["broadcast"])?;
    let admin_message = "Admin broadcast message";

    let admin_broadcast_event = EventBuilder::new(Kind::Custom(9), admin_message)
        .tag(group_tag.clone())
        .tag(broadcast_tag.clone())
        .sign(&admin_keys)
        .await?;

    // Clear previous messages
    fcm_mock.clear();
    
    // Send the admin broadcast
    admin_client.send_event(&admin_broadcast_event).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify notifications were sent (admin broadcast allowed)
    let admin_sent_messages = fcm_mock.get_sent_messages();
    assert_eq!(
        admin_sent_messages.len(),
        1,
        "Expected 1 notification from admin broadcast"
    );
    assert_eq!(
        admin_sent_messages[0].0,
        member_token.to_string(),
        "Expected notification to be sent to member"
    );

    // 2. Test broadcast from non-admin user (should be rejected)
    let non_admin_message = "Non-admin broadcast message attempt";

    let non_admin_broadcast_event = EventBuilder::new(Kind::Custom(9), non_admin_message)
        .tag(group_tag.clone())
        .tag(broadcast_tag.clone())
        .sign(&non_admin_keys)
        .await?;

    // Clear previous messages
    fcm_mock.clear();
    
    // Send the non-admin broadcast
    non_admin_client.send_event(&non_admin_broadcast_event).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify no notifications were sent (non-admin broadcast rejected)
    let non_admin_sent_messages = fcm_mock.get_sent_messages();
    assert_eq!(
        non_admin_sent_messages.len(),
        0,
        "Expected 0 notifications from non-admin broadcast (should be rejected)"
    );

    // Clean up
    admin_client.disconnect().await;
    non_admin_client.disconnect().await;
    member_client.disconnect().await;
    service_token.cancel();
    let _ = listener_handle.await;
    let _ = handler_handle.await;

    Ok(())
}

/// Test 6: Event kind filtering for broadcast messages
#[tokio::test]
#[serial]
async fn test_event_kind_filtering() -> Result<()> {
    let (state, fcm_mock, relay_url, _mock_relay) = setup_broadcast_test_environment().await?;

    // Set up service
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

    // Create test group
    let group_id = "test_kind_filtering_group";
    
    // Admin user who will send different kinds of events
    let admin_keys = Keys::generate();
    let admin_client = ClientBuilder::new().signer(admin_keys.clone()).build();
    admin_client.add_relay(relay_url.as_str()).await?;
    admin_client.connect().await;
    
    // Regular member to receive notifications
    let member_keys = Keys::generate();
    let member_client = ClientBuilder::new().signer(member_keys.clone()).build();
    member_client.add_relay(relay_url.as_str()).await?;
    member_client.connect().await;

    // Register FCM token for member
    let member_token = "fcm_token_kind_filter_member";
    register_user_token(&state, &member_keys, &member_client, &member_token).await?;

    // Define admins and members
    let mut admins = HashSet::new();
    admins.insert(admin_keys.public_key());
    
    let mut members = HashSet::new();
    members.insert(admin_keys.public_key());
    members.insert(member_keys.public_key());

    // Set up test group
    setup_test_group_with_admin(&state, group_id, admins.clone(), members.clone()).await?;

    let group_tag = Tag::parse(["h", &group_id])?;
    let broadcast_tag = Tag::parse(["broadcast"])?;

    // 1. Test with ALLOWED kind 11 (broadcastable) - should send notification
    let allowed_message = "Allowed event kind broadcast";
    let allowed_kind_event = EventBuilder::new(Kind::Custom(9), allowed_message)
        .tag(group_tag.clone())
        .tag(broadcast_tag.clone())
        .sign(&admin_keys)
        .await?;

    // Clear previous messages
    fcm_mock.clear();
    
    // Send the allowed kind event
    admin_client.send_event(&allowed_kind_event).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify notification was sent (allowed kind)
    let allowed_sent_messages = fcm_mock.get_sent_messages();
    assert_eq!(
        allowed_sent_messages.len(),
        1,
        "Expected 1 notification from allowed kind (9) broadcast"
    );

    // 2. Test with DISALLOWED kind 12 (not broadcastable) - should NOT send notification
    let disallowed_reply_message = "Disallowed reply event kind broadcast";
    let disallowed_reply_event = EventBuilder::new(Kind::Custom(12), disallowed_reply_message)
        .tag(group_tag.clone())
        .tag(broadcast_tag.clone())
        .sign(&admin_keys)
        .await?;

    // Clear previous messages
    fcm_mock.clear();
    
    // Send the disallowed reply kind event
    admin_client.send_event(&disallowed_reply_event).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify notification was NOT sent (disallowed kind)
    let disallowed_reply_sent_messages = fcm_mock.get_sent_messages();
    assert_eq!(
        disallowed_reply_sent_messages.len(),
        0,
        "Expected 0 notifications from disallowed kind (12) broadcast"
    );

    // 3. Test with ANOTHER DISALLOWED kind (e.g., Kind 42) - should NOT send notification
    let disallowed_message = "Disallowed event kind broadcast attempt";
    let disallowed_kind_event = EventBuilder::new(Kind::Custom(42), disallowed_message)
        .tag(group_tag.clone())
        .tag(broadcast_tag.clone())
        .sign(&admin_keys)
        .await?;

    // Clear previous messages
    fcm_mock.clear();
    
    // Send the disallowed kind event
    admin_client.send_event(&disallowed_kind_event).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify no notification was sent (disallowed kind)
    let disallowed_sent_messages = fcm_mock.get_sent_messages();
    assert_eq!(
        disallowed_sent_messages.len(),
        0,
        "Expected 0 notifications from disallowed kind (42) broadcast"
    );

    // Clean up
    admin_client.disconnect().await;
    member_client.disconnect().await;
    service_token.cancel();
    let _ = listener_handle.await;
    let _ = handler_handle.await;

    Ok(())
}
