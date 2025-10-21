use anyhow::{anyhow, Result};
use nostr_relay_builder::MockRelay;
// Removed serial_test - tests now run in parallel with isolated Redis databases

mod common;
use nostr_sdk::{ClientBuilder, Event, EventBuilder, Keys, Kind, PublicKey, Tag, TagKind, ToBech32};
use nostr_push_service::{
    config::Settings,
    redis_store::{self, RedisPool},
    state::AppState,
};
use std::collections::HashSet;
use std::sync::Arc;

use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::Url;


// Import service components
use nostr_push_service::{event_handler, nostr_listener};
use nostr_push_service::event_handler::EventContext;
use tokio::sync::mpsc;

/// Test environment setup, similar to integration_test.rs but focused on broadcast tests
async fn setup_broadcast_test_environment() -> Result<(
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

    // For testing purposes, use a hardcoded test key in hex format
    let test_key_hex = "0000000000000000000000000000000000000000000000000000000000000001";
    std::env::set_var("NOSTR_PUSH__SERVICE__PRIVATE_KEY_HEX", test_key_hex);

    let mut settings = Settings::new()
        .map_err(|e| anyhow!("Failed to load settings after setting env var: {}", e))?;
    let test_service_keys = Keys::generate();

    let mock_relay = MockRelay::run().await?;
    let relay_url_str = mock_relay.url();
    let relay_url = Url::parse(&relay_url_str)?;

    settings.nostr.relay_url = relay_url_str.clone();
    
    // Configure the nostrpushdemo app for tests
    settings.apps = vec![nostr_push_service::config::AppConfig {
        name: "nostrpushdemo".to_string(),
        frontend_config: nostr_push_service::config::FrontendConfig {
            api_key: "test-api-key".to_string(),
            auth_domain: "test.firebaseapp.com".to_string(),
            project_id: "test-project".to_string(),
            storage_bucket: "test.firebasestorage.app".to_string(),
            messaging_sender_id: "123456".to_string(),
            app_id: "1:123456:web:test".to_string(),
            measurement_id: None,
            vapid_public_key: "test-vapid-key".to_string(),
        },
        credentials_path: "./test-firebase-credentials.json".to_string(),
        allowed_subscription_kinds: vec![],
    }];

    // Allow time for Redis to initialize
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Create Redis pool
    let redis_pool = nostr_push_service::redis_store::create_pool(
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
    let mock_fcm_sender_instance = nostr_push_service::fcm_sender::MockFcmSender::new();
    let mock_fcm_sender_arc = Arc::new(mock_fcm_sender_instance.clone());

    // Create FcmClient wrapper with the mock implementation
    let fcm_client = Arc::new(nostr_push_service::fcm_sender::FcmClient::new_with_impl(
        Box::new(mock_fcm_sender_instance),
    ));

    // Initialize Nip29Client for the AppState
    let nip29_cache_expiration = settings.nostr.cache_expiration.unwrap_or(300);
    eprintln!("Creating Nip29Client for relay: {}", relay_url_str);
    let nip29_client = nostr_push_service::nostr::nip29::Nip29Client::new(
        relay_url_str.clone(),
        test_service_keys.clone(),
        nip29_cache_expiration,
    )
    .await
    .map_err(|e| anyhow!("Failed to create and connect Nip29Client: {}", e))?;
    eprintln!("Nip29Client created successfully");

    // Add a small delay to ensure the client has time to establish the connection
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Create FCM clients map for the configured app
    let mut fcm_clients = std::collections::HashMap::new();
    let mut supported_apps = std::collections::HashSet::new();
    fcm_clients.insert("nostrpushdemo".to_string(), fcm_client);
    supported_apps.insert("nostrpushdemo".to_string());
    
    let (subscription_manager, community_handler) = common::create_default_handlers();
    
    // Get the nostr client from nip29_client  
    let nostr_client = nip29_client.client();
    
    // Initialize the shared user subscriptions map
    let user_subscriptions = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    
    let app_state = nostr_push_service::state::AppState {
        settings,
        redis_pool,
        fcm_clients,
        supported_apps,
        service_keys: Some(test_service_keys.clone()),
        crypto_service: Some(nostr_push_service::crypto::CryptoService::new(test_service_keys)),
        nip29_client: Arc::new(nip29_client),
        nostr_client,
        user_subscriptions,
        subscription_manager,
        community_handler,
        notification_config: None,
    };

    Ok((
        Arc::new(app_state),
        mock_fcm_sender_arc,
        relay_url,
        mock_relay,
    ))
}

async fn cleanup_redis(pool: &RedisPool) -> Result<()> {
    common::clean_redis_globals(pool).await
}

/// Helper to register a user's FCM token
async fn register_user_token(
    state: &Arc<AppState>,
    user_keys: &Keys,
    client: &nostr_sdk::Client,
    token: &str,
) -> Result<()> {
    // Get service public key from state
    let service_pubkey = state.service_keys
        .as_ref()
        .ok_or_else(|| anyhow!("Service keys not configured"))?
        .public_key();
    
    // Create token payload
    let payload = serde_json::json!({ "token": token }).to_string();
    
    // Encrypt the payload using NIP-44
    let encrypted = nostr_sdk::nips::nip44::encrypt(
        user_keys.secret_key(),
        &service_pubkey,
        payload,
        nostr_sdk::nips::nip44::Version::V2,
    )?;
    
    // Use standard NIP-44 format (no prefix)
    let encrypted_content = encrypted.clone();
    
    // Create registration event with app tag for nostrpushdemo
    let registration_event = EventBuilder::new(Kind::Custom(3079), encrypted_content)
        .tag(Tag::public_key(service_pubkey))
        .tag(Tag::custom(TagKind::Custom("app".into()), vec!["nostrpushdemo".to_string()]))
        .tag(Tag::parse(["expiration", &(nostr_sdk::Timestamp::now().as_u64() + 86400).to_string()])?)
        .sign(user_keys)
        .await?;
    
    client.send_event(&registration_event).await?;
    
    // Poll for token to be registered instead of fixed wait
    let token_string = token.to_string();
    let user_pubkey = user_keys.public_key();
    let state_clone = state.clone();
    
    let result = common::wait_for_condition(
        || async {
            // Check nostrpushdemo namespace
            let stored_tokens = redis_store::get_tokens_for_pubkey_with_app(&state_clone.redis_pool, "nostrpushdemo", &user_pubkey)
                .await
                .unwrap_or_default();
            stored_tokens.contains(&token_string)
        },
        Duration::from_millis(100),  // Poll every 100ms
        Duration::from_secs(5),       // Timeout after 5 seconds
    ).await;
    
    if result.is_err() {
        // Debug output on timeout
        let stored_tokens =
            redis_store::get_tokens_for_pubkey_with_app(&state.redis_pool, "nostrpushdemo", &user_keys.public_key()).await?;
        eprintln!(
            "Timeout: Token '{}' not found. User has {} tokens: {:?}",
            token,
            stored_tokens.len(),
            stored_tokens
        );
        return Err(anyhow!("Token {} was not registered within timeout", token));
    }

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
#[ignore = "Needs update for v2 event handler architecture"]
async fn test_broadcast_notifications() -> Result<()> {
    let (state, fcm_mock, relay_url, _mock_relay) = setup_broadcast_test_environment().await?;
    
    // Clear any messages that might have been sent during setup
    fcm_mock.clear();

    // Set up service with event handler and listener
    let service_token = CancellationToken::new();
    let (event_tx, event_rx) = mpsc::channel::<(Box<Event>, EventContext)>(100);

    // Start nostr listener
    let listener_state = state.clone();
    let listener_token = service_token.clone();
    let listener_handle = tokio::spawn(async move {
        eprintln!("Starting nostr_listener task...");
        let listener = nostr_listener::NostrListener::new(listener_state);
        if let Err(e) = listener.run(event_tx, listener_token).await {
            eprintln!("Nostr listener task error: {}", e);
        }
        eprintln!("Nostr listener task ended");
    });

    // Start event handler
    let handler_state = state.clone();
    let handler_token = service_token.clone();
    let handler_handle = tokio::spawn(async move {
        if let Err(e) = event_handler::run(handler_state, event_rx, handler_token).await {
            eprintln!("Event handler task error: {}", e);
        }
    });

    tokio::time::sleep(Duration::from_secs(5)).await;  // Wait longer for services to connect to relay

    // Create test group members
    let group_id = "test_broadcast_group";
    let sender_keys = Keys::generate();
    let sender_client = ClientBuilder::new().signer(sender_keys.clone()).build();
    eprintln!("Test client connecting to relay: {}", relay_url.as_str());
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

    // Register FCM tokens for each user with unique IDs
    let test_id = common::get_unique_test_id();
    let token1 = format!("fcm_token_user1_broadcast_{}", test_id);
    let token2 = format!("fcm_token_user2_broadcast_{}", test_id);
    let token3 = format!("fcm_token_user3_broadcast_{}", test_id);

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
    let group_tag = Tag::parse(["h", group_id])?;
    let broadcast_tag = Tag::parse(["broadcast"])?;
    let message_content = "This is a broadcast message to the group";

    let broadcast_event = EventBuilder::new(Kind::Custom(9), message_content)
        .tag(group_tag)
        .tag(broadcast_tag)
        .sign(&sender_keys)
        .await?;

    sender_client.send_event(&broadcast_event).await?;

    // Poll for FCM messages to be sent instead of fixed wait
    let expected_count = 3;  // 3 users should receive notifications
    
    let _ = common::wait_for_condition(
        || {
            let fcm_mock_clone = fcm_mock.clone();
            async move {
                fcm_mock_clone.get_sent_messages().len() >= expected_count
            }
        },
        Duration::from_millis(100),  // Poll every 100ms
        Duration::from_secs(2),       // Timeout after 2 seconds
    ).await;  // Ignore timeout - we'll check the actual count below

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
        sent_tokens.contains(&&token1),
        "Expected user1 token in FCM messages"
    );
    assert!(
        sent_tokens.contains(&&token2),
        "Expected user2 token in FCM messages"
    );
    assert!(
        sent_tokens.contains(&&token3),
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
#[ignore = "Needs update for v2 event handler architecture"]
async fn test_regular_mentions_with_broadcast() -> Result<()> {
    let (state, fcm_mock, relay_url, _mock_relay) = setup_broadcast_test_environment().await?;
    
    // Clear any messages that might have been sent during setup
    fcm_mock.clear();

    // Set up service
    let service_token = CancellationToken::new();
    let (event_tx, event_rx) = mpsc::channel::<(Box<Event>, EventContext)>(100);

    let listener_state = state.clone();
    let listener_token = service_token.clone();
    let _listener_handle = tokio::spawn(async move {
        let listener = nostr_listener::NostrListener::new(listener_state);
        if let Err(e) = listener.run(event_tx, listener_token).await {
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

    tokio::time::sleep(Duration::from_secs(2)).await;  // Wait for service to be ready

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

    // Register FCM tokens with unique IDs
    let test_id = common::get_unique_test_id();
    let token1 = format!("fcm_token_user1_mention_{}", test_id);
    let token2 = format!("fcm_token_user2_mention_{}", test_id);
    let token3 = format!("fcm_token_user3_mention_{}", test_id);

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
    let group_tag = Tag::parse(["h", group_id])?;
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
        token1,
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
        sent_tokens.contains(&&token1),
        "Expected user1 token in FCM messages"
    );
    assert!(
        sent_tokens.contains(&&token2),
        "Expected user2 token in FCM messages"
    );
    assert!(
        sent_tokens.contains(&&token3),
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
#[ignore = "Needs update for v2 event handler architecture"]
async fn test_broadcast_performance_large_group() -> Result<()> {
    let (state, fcm_mock, relay_url, _mock_relay) = setup_broadcast_test_environment().await?;
    
    // Clear any messages that might have been sent during setup
    fcm_mock.clear();

    // Set up service
    let service_token = CancellationToken::new();
    let (event_tx, event_rx) = mpsc::channel::<(Box<Event>, EventContext)>(100);

    let listener_state = state.clone();
    let listener_token = service_token.clone();
    let listener_handle = tokio::spawn(async move {
        eprintln!("Starting nostr_listener task...");
        let listener = nostr_listener::NostrListener::new(listener_state);
        if let Err(e) = listener.run(event_tx, listener_token).await {
            eprintln!("Nostr listener task error: {}", e);
        }
        eprintln!("Nostr listener task ended");
    });

    let handler_state = state.clone();
    let handler_token = service_token.clone();
    let handler_handle = tokio::spawn(async move {
        if let Err(e) = event_handler::run(handler_state, event_rx, handler_token).await {
            eprintln!("Event handler task error: {}", e);
        }
    });

    tokio::time::sleep(Duration::from_secs(2)).await;  // Wait for service to be ready

    // Create unique test ID for this test
    let test_id_base = common::get_unique_test_id();
    
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
async fn test_broadcast_tag_detection() -> Result<()> {
    // Create unique test ID for this test
    let test_id = common::get_unique_test_id();
    
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
#[ignore = "Needs update for v2 event handler architecture"]
async fn test_admin_permission_verification() -> Result<()> {
    let (state, fcm_mock, relay_url, _mock_relay) = setup_broadcast_test_environment().await?;
    
    // Clear any messages that might have been sent during setup
    fcm_mock.clear();

    // Set up service
    let service_token = CancellationToken::new();
    let (event_tx, event_rx) = mpsc::channel::<(Box<Event>, EventContext)>(100);

    let listener_state = state.clone();
    let listener_token = service_token.clone();
    let listener_handle = tokio::spawn(async move {
        eprintln!("Starting nostr_listener task...");
        let listener = nostr_listener::NostrListener::new(listener_state);
        if let Err(e) = listener.run(event_tx, listener_token).await {
            eprintln!("Nostr listener task error: {}", e);
        }
        eprintln!("Nostr listener task ended");
    });

    let handler_state = state.clone();
    let handler_token = service_token.clone();
    let handler_handle = tokio::spawn(async move {
        if let Err(e) = event_handler::run(handler_state, event_rx, handler_token).await {
            eprintln!("Event handler task error: {}", e);
        }
    });

    tokio::time::sleep(Duration::from_secs(2)).await;  // Wait for service to be ready

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

    // Register FCM token for member (to receive notifications) with unique ID
    let test_id = common::get_unique_test_id();
    let member_token = format!("fcm_token_group_member_{}", test_id);
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
    let group_tag = Tag::parse(["h", group_id])?;
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
        member_token,
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
#[ignore = "Needs update for v2 event handler architecture"]
async fn test_event_kind_filtering() -> Result<()> {
    let (state, fcm_mock, relay_url, _mock_relay) = setup_broadcast_test_environment().await?;
    
    // Clear any messages that might have been sent during setup
    fcm_mock.clear();

    // Set up service
    let service_token = CancellationToken::new();
    let (event_tx, event_rx) = mpsc::channel::<(Box<Event>, EventContext)>(100);

    let listener_state = state.clone();
    let listener_token = service_token.clone();
    let listener_handle = tokio::spawn(async move {
        eprintln!("Starting nostr_listener task...");
        let listener = nostr_listener::NostrListener::new(listener_state);
        if let Err(e) = listener.run(event_tx, listener_token).await {
            eprintln!("Nostr listener task error: {}", e);
        }
        eprintln!("Nostr listener task ended");
    });

    let handler_state = state.clone();
    let handler_token = service_token.clone();
    let handler_handle = tokio::spawn(async move {
        if let Err(e) = event_handler::run(handler_state, event_rx, handler_token).await {
            eprintln!("Event handler task error: {}", e);
        }
    });

    tokio::time::sleep(Duration::from_secs(2)).await;  // Wait for service to be ready

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

    // Register FCM token for member with unique ID
    let test_id = common::get_unique_test_id();
    let member_token = format!("fcm_token_kind_filter_member_{}", test_id);
    register_user_token(&state, &member_keys, &member_client, &member_token).await?;

    // Define admins and members
    let mut admins = HashSet::new();
    admins.insert(admin_keys.public_key());
    
    let mut members = HashSet::new();
    members.insert(admin_keys.public_key());
    members.insert(member_keys.public_key());

    // Set up test group
    setup_test_group_with_admin(&state, group_id, admins.clone(), members.clone()).await?;

    let group_tag = Tag::parse(["h", group_id])?;
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
