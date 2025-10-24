mod common;
use nostr_push_service::{
    config::Settings,
    error::ServiceError,
    event_handler,
    fcm_sender::{FcmClient, MockFcmSender},
    redis_store,
    state::AppState,
    crypto::CryptoService,
};
use nostr_sdk::prelude::*;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;
use redis::AsyncCommands;

/// Clean up app-specific data from Redis
async fn cleanup_app_data(pool: &redis_store::RedisPool, app: &str) -> Result<(), redis::RedisError> {
    let mut conn = pool.get().await.map_err(|e| {
        redis::RedisError::from((redis::ErrorKind::IoError, "Failed to get connection", e.to_string()))
    })?;
    
    // Delete app-specific keys using pattern matching
    let pattern = format!("app:{}:*", app);
    let keys: Vec<String> = conn.keys(&pattern).await?;
    
    if !keys.is_empty() {
        // Explicitly specify type to avoid fallback warning
        let _: () = conn.del(keys).await?;
    }
    
    Ok(())
}

/// Create test settings with provided service private key
fn create_test_settings(service_private_hex: String) -> Settings {
    // Create settings directly without using environment variables to avoid race conditions
    Settings {
        service: nostr_push_service::config::ServiceSettings {
            private_key_hex: Some(service_private_hex),
            processed_event_ttl_secs: 3600,
            process_window_days: 7,
            control_kinds: vec![3079, 3080, 3081, 3082],
            dm_kinds: vec![1059, 14],
        },
        redis: nostr_push_service::config::RedisSettings {
            url: "redis://localhost:6379".to_string(),
            connection_pool_size: 10,
        },
        apps: vec![nostr_push_service::config::AppConfig {
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
            allowed_subscription_kinds: vec![1, 3, 7, 1111, 34550],
        }],
        nostr: nostr_push_service::config::NostrSettings {
            relay_url: "wss://test.relay".to_string(),
            cache_expiration: Some(300),
        },
        cleanup: nostr_push_service::config::CleanupSettings {
            enabled: false,
            interval_secs: 3600,
            token_max_age_days: 30,
        },
        server: nostr_push_service::config::ServerSettings {
            listen_addr: "127.0.0.1:8080".to_string(),
        },
        notification: None,
    }
}

/// Create AppState with mock FCM sender for tests
async fn create_test_app_state(settings: Settings, cleanup_app: Option<&str>) -> Arc<AppState> {
    // Create Redis pool
    let redis_pool = redis_store::create_pool(
        &settings.redis.url,
        settings.redis.connection_pool_size,
    )
    .await
    .expect("Failed to create Redis pool");
    
    // Clean up any existing data for this app to avoid conflicts
    if let Some(app) = cleanup_app {
        // Try to clean up, but don't fail if it errors
        let _ = cleanup_app_data(&redis_pool, app).await;
    }
    
    // Create mock FCM clients for each configured app
    let mut fcm_clients = std::collections::HashMap::new();
    let mut supported_apps = std::collections::HashSet::new();
    
    for app_config in &settings.apps {
        let mock_fcm_sender = MockFcmSender::new();
        let fcm_client = Arc::new(FcmClient::new_with_impl(Box::new(mock_fcm_sender)));
        fcm_clients.insert(app_config.name.clone(), fcm_client);
        supported_apps.insert(app_config.name.clone());
    }
    
    // Get service keys
    let service_keys = settings.get_service_keys();
    
    // Create crypto service if we have service keys
    let crypto_service = service_keys.as_ref().map(|keys| {
        CryptoService::new(keys.clone())
    });
    
    // Create a minimal Nip29Client using from_settings which doesn't auto-connect
    let nip29_client = nostr_push_service::nostr::nip29::Nip29Client::from_settings(&settings)
        .expect("Failed to create Nip29Client from settings");
    
    let (subscription_manager, community_handler) = common::create_default_handlers();
    
    // Get the nostr client from nip29_client  
    let nostr_client = nip29_client.client();
    
    // Add a relay to the client for subscription management (using settings relay URL)
    // This is needed for the subscription manager to work properly
    let _ = nostr_client.add_relay(&settings.nostr.relay_url).await;
    
    // Initialize the shared user subscriptions map
    let user_subscriptions = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    
    Arc::new(AppState {
        settings,
        redis_pool,
        fcm_clients,
        supported_apps,
        service_keys,
        crypto_service,
        nip29_client: Arc::new(nip29_client),
        nostr_client: nostr_client.clone(),
        profile_client: nostr_client.clone(),
        user_subscriptions,
        subscription_manager,
        community_handler,
        notification_config: None,
    })
}

/// Generate test keypair for service
fn generate_service_keypair() -> (String, PublicKey) {
    let keys = Keys::generate();
    let private_hex = keys.secret_key().display_secret().to_string();
    let public_key = keys.public_key();
    (private_hex, public_key)
}

/// Generate test keypair for user
fn generate_user_keypair() -> (SecretKey, PublicKey) {
    let keys = Keys::generate();
    (keys.secret_key().clone(), keys.public_key())
}

/// Create a NIP-44 encrypted token payload
async fn encrypt_token(token: &str, sender_sk: &SecretKey, recipient_pubkey: &PublicKey) -> String {
    let payload = json!({ "token": token }).to_string();
    
    // Use NIP-44 encryption
    let encrypted = nip44::encrypt(
        sender_sk,
        recipient_pubkey,
        payload,
        nip44::Version::V2,
    ).expect("Failed to encrypt");
    
    // Return standard NIP-44 format (no prefix)
    encrypted
}

/// Create a test event for registration (3079) or deregistration (3080)
fn create_test_event(
    kind: Kind,
    content: String,
    sender_keys: &Keys,
    service_pubkey: &PublicKey,
    app_name: &str,
) -> Event {
    let mut builder = EventBuilder::new(kind, content);
    
    // Add tags
    builder = builder.tag(Tag::public_key(*service_pubkey)); // p tag
    builder = builder.tag(Tag::custom(TagKind::Custom("app".into()), vec![app_name.to_string()])); // app tag
    
    if kind == Kind::Custom(3079) {
        // Add expiration tag for registration
        let expiration = Timestamp::now() + 86400; // 1 day
        builder = builder.tag(Tag::expiration(expiration));
    }
    
    builder
        .sign_with_keys(sender_keys)
        .expect("Failed to sign event")
}

/// Create subscription event (3081 or 3082)
fn create_subscription_event(
    kind: Kind,
    filter: serde_json::Value,
    sender_keys: &Keys,
    service_pubkey: &PublicKey,
    app_name: &str,
) -> Event {
    // Prepare the payload based on event kind
    let payload = if kind == Kind::Custom(3081) {
        // For upsert, create a full filter payload
        serde_json::json!({
            "filter": filter
        })
    } else {
        // For delete (3082), send the same filter to remove
        serde_json::json!({
            "filter": filter
        })
    };
    
    // Encrypt the payload using NIP-44
    let encrypted = nip44::encrypt(
        sender_keys.secret_key(),
        service_pubkey,
        payload.to_string(),
        nip44::Version::V2,
    ).expect("Failed to encrypt");
    
    EventBuilder::new(kind, encrypted)
        .tag(Tag::public_key(*service_pubkey)) // p tag
        .tag(Tag::custom(TagKind::Custom("app".into()), vec![app_name.to_string()])) // app tag
        .sign_with_keys(sender_keys)
        .expect("Failed to sign event")
}

/// Helper to process an event and check if it's accepted
async fn process_event(
    app_state: Arc<AppState>,
    event: Event,
) -> Result<(), ServiceError> {
    let (tx, rx) = mpsc::channel::<(Box<Event>, event_handler::EventContext)>(1);
    
    // Send event for processing
    let context = event_handler::EventContext::Live; // Use Live context for testing
    tx.send((Box::new(event.clone()), context))
    .await
    .expect("Failed to send event");
    
    drop(tx);
    
    // Process the event handler synchronously
    let token = tokio_util::sync::CancellationToken::new();
    
    // Run the handler and wait for it to complete
    let result = event_handler::run(app_state, rx, token).await;
    
    // Give extra time for any async Redis operations to complete
    tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
    
    result
}

#[tokio::test]
async fn test_encrypted_token_registration_works() {
    // Setup
    let (service_private_hex, service_pubkey) = generate_service_keypair();
    let (user_sk, user_pubkey) = generate_user_keypair();
    let user_keys = Keys::new(user_sk.clone());
    
    // Create test settings
    let settings = create_test_settings(service_private_hex.clone());
    
    // Use the configured app name
    let app_name = "nostrpushdemo";
    
    // Create app state with mock FCM and cleanup existing data
    let app_state = create_test_app_state(settings, Some(&app_name)).await;
    
    // Create encrypted token
    let token = "test_fcm_token_12345";
    let encrypted_content = encrypt_token(token, &user_sk, &service_pubkey).await;
    
    // Create registration event with encrypted content
    let event = create_test_event(
        Kind::Custom(3079),
        encrypted_content,
        &user_keys,
        &service_pubkey,
        &app_name,
    );
    
    // Process the event
    eprintln!("Processing event with:");
    eprintln!("  Event ID: {}", event.id);
    eprintln!("  Event kind: {}", event.kind);
    eprintln!("  Event pubkey: {}", event.pubkey);
    eprintln!("  Service pubkey from state: {:?}", app_state.service_keys.as_ref().map(|k| k.public_key()));
    eprintln!("  Event p-tags: {:?}", event.tags.iter().filter(|t| t.kind() == nostr_sdk::TagKind::p()).collect::<Vec<_>>());
    eprintln!("  Event content starts with: {}", &event.content[..50.min(event.content.len())]);
    eprintln!("  Event all tags: {:?}", event.tags);
    
    let result = process_event(app_state.clone(), event).await;
    assert!(result.is_ok(), "Encrypted token registration should succeed: {:?}", result);
    
    // Verify token was stored in Redis
    let stored_tokens = redis_store::get_tokens_for_pubkey_with_app(
        &app_state.redis_pool,
        &app_name,  // Use the same unique app name
        &user_pubkey
    )
    .await
    .expect("Failed to get tokens");
    
    eprintln!("Test: test_encrypted_token_registration_works");
    eprintln!("  App name: {}", app_name);
    eprintln!("  User pubkey: {}", user_pubkey);
    eprintln!("  Expected token: {}", token);
    eprintln!("  Stored tokens: {:?}", stored_tokens);
    
    assert_eq!(stored_tokens.len(), 1, "Expected 1 token for app {}, got {}: {:?}", app_name, stored_tokens.len(), stored_tokens);
    assert_eq!(stored_tokens[0], token);
}

#[tokio::test]
async fn test_plaintext_token_rejected() {
    // Setup
    let (service_private_hex, service_pubkey) = generate_service_keypair();
    let (user_sk, user_pubkey) = generate_user_keypair();
    let user_keys = Keys::new(user_sk);
    
    // Create test settings
    let settings = create_test_settings(service_private_hex);
    
    // Use the configured app name
    let app_name = "nostrpushdemo";
    
    // Create app state with mock FCM and cleanup existing data
    let app_state = create_test_app_state(settings, Some(&app_name)).await;
    
    // Create event with plaintext token (should be rejected)
    let event = create_test_event(
        Kind::Custom(3079),
        "plaintext_fcm_token".to_string(), // NOT encrypted
        &user_keys,
        &service_pubkey,
        &app_name,
    );
    
    // Process the event
    let result = process_event(app_state.clone(), event).await;
    assert!(result.is_ok(), "Processing should complete without error");
    
    // Verify token was NOT stored
    let stored_tokens = redis_store::get_tokens_for_pubkey_with_app(
        &app_state.redis_pool,
        &app_name,
        &user_pubkey
    )
    .await
    .expect("Failed to get tokens");
    
    assert_eq!(stored_tokens.len(), 0, "Plaintext token should be rejected");
}

#[tokio::test]
async fn test_multi_app_namespace_isolation() {
    // Setup
    let (service_private_hex, service_pubkey) = generate_service_keypair();
    let (user_sk, user_pubkey) = generate_user_keypair();
    let user_keys = Keys::new(user_sk.clone());
    
    // Create test settings with multiple apps for this test
    let mut settings = create_test_settings(service_private_hex.clone());
    
    // Use test app names (we'll create different configs for this test)
    let app1_name = "testapp1";
    let app2_name = "testapp2";
    
    // Override the apps configuration to include both test apps
    settings.apps = vec![
        nostr_push_service::config::AppConfig {
            name: app1_name.to_string(),
            frontend_config: nostr_push_service::config::FrontendConfig {
                api_key: "test-api-key".to_string(),
                auth_domain: "test.firebaseapp.com".to_string(),
                project_id: "test-project-1".to_string(),
                storage_bucket: "test.firebasestorage.app".to_string(),
                messaging_sender_id: "123456".to_string(),
                app_id: "1:123456:web:test".to_string(),
                measurement_id: None,
                vapid_public_key: "test-vapid-key".to_string(),
            },
            credentials_path: "./test-firebase-credentials.json".to_string(),
            allowed_subscription_kinds: vec![],
        },
        nostr_push_service::config::AppConfig {
            name: app2_name.to_string(),
            frontend_config: nostr_push_service::config::FrontendConfig {
                api_key: "test-api-key".to_string(),
                auth_domain: "test.firebaseapp.com".to_string(),
                project_id: "test-project-2".to_string(),
                storage_bucket: "test.firebasestorage.app".to_string(),
                messaging_sender_id: "123456".to_string(),
                app_id: "1:123456:web:test".to_string(),
                measurement_id: None,
                vapid_public_key: "test-vapid-key".to_string(),
            },
            credentials_path: "./test-firebase-credentials.json".to_string(),
            allowed_subscription_kinds: vec![],
        },
    ];
    
    // Create app state with mock FCM and cleanup both app namespaces
    let app_state = create_test_app_state(settings, None).await;
    // Clean up both apps manually
    let _ = cleanup_app_data(&app_state.redis_pool, &app1_name).await;
    let _ = cleanup_app_data(&app_state.redis_pool, &app2_name).await;
    
    // Register tokens for different apps
    let token_app1 = "token_for_app1";
    let token_app2 = "token_for_app2";
    
    // Register for app1
    let encrypted_app1 = encrypt_token(token_app1, &user_sk, &service_pubkey).await;
    let event_app1 = create_test_event(
        Kind::Custom(3079),
        encrypted_app1,
        &user_keys,
        &service_pubkey,
        &app1_name,
    );
    process_event(app_state.clone(), event_app1).await.expect("App1 registration failed");
    
    // Register for app2
    let encrypted_app2 = encrypt_token(token_app2, &user_sk, &service_pubkey).await;
    let event_app2 = create_test_event(
        Kind::Custom(3079),
        encrypted_app2,
        &user_keys,
        &service_pubkey,
        &app2_name,
    );
    process_event(app_state.clone(), event_app2).await.expect("App2 registration failed");
    
    // Verify tokens are isolated by app
    let tokens_app1 = redis_store::get_tokens_for_pubkey_with_app(
        &app_state.redis_pool,
        &app1_name,
        &user_pubkey
    )
    .await
    .expect("Failed to get app1 tokens");
    
    let tokens_app2 = redis_store::get_tokens_for_pubkey_with_app(
        &app_state.redis_pool,
        &app2_name,
        &user_pubkey
    )
    .await
    .expect("Failed to get app2 tokens");
    
    assert_eq!(tokens_app1.len(), 1);
    assert_eq!(tokens_app1[0], token_app1);
    
    assert_eq!(tokens_app2.len(), 1);
    assert_eq!(tokens_app2[0], token_app2);
    
    // Verify apps don't see each other's tokens
    assert_ne!(tokens_app1[0], tokens_app2[0]);
}

#[tokio::test]
async fn test_service_filtering_by_p_tag() {
    // Setup two different services
    let (service1_private_hex, _service1_pubkey) = generate_service_keypair();
    let (_service2_private_hex, service2_pubkey) = generate_service_keypair();
    let (user_sk, user_pubkey) = generate_user_keypair();
    let user_keys = Keys::new(user_sk.clone());
    
    // Create test settings for service1
    let settings = create_test_settings(service1_private_hex);
    
    // Use the configured app name
    let app_name = "nostrpushdemo";
    
    // Create app state for service1 with cleanup
    let app_state = create_test_app_state(settings, Some(&app_name)).await;
    
    // Create event targeted at service2 (different service)
    let token = "token_for_service2";
    let encrypted_content = encrypt_token(token, &user_sk, &service2_pubkey).await;
    let event = create_test_event(
        Kind::Custom(3079),
        encrypted_content,
        &user_keys,
        &service2_pubkey, // Different service!
        &app_name,
    );
    
    // Process the event - should be ignored by service1
    let result = process_event(app_state.clone(), event).await;
    assert!(result.is_ok());
    
    // Verify token was NOT stored (wrong service)
    let stored_tokens = redis_store::get_tokens_for_pubkey_with_app(
        &app_state.redis_pool,
        &app_name,
        &user_pubkey
    )
    .await
    .expect("Failed to get tokens");
    
    assert_eq!(stored_tokens.len(), 0, "Token for different service should be ignored");
}

#[tokio::test]
async fn test_subscription_with_proper_tags() {
    // Setup
    let (service_private_hex, service_pubkey) = generate_service_keypair();
    let (user_sk, user_pubkey) = generate_user_keypair();
    let user_keys = Keys::new(user_sk.clone());
    
    // Create test settings
    let settings = create_test_settings(service_private_hex.clone());
    
    // Use the configured app name
    let app_name = "nostrpushdemo";
    
    // Create app state with mock FCM and cleanup existing data
    let app_state = create_test_app_state(settings, Some(&app_name)).await;
    
    // First register a token
    let token = "subscription_test_token";
    let encrypted_token = encrypt_token(token, &user_sk, &service_pubkey).await;
    let reg_event = create_test_event(
        Kind::Custom(3079),
        encrypted_token,
        &user_keys,
        &service_pubkey,
        &app_name,
    );
    process_event(app_state.clone(), reg_event).await.expect("Registration failed");
    
    // Create subscription filter with allowed kinds
    let filter = json!({
        "kinds": [1, 3], // Text notes and contact lists (both are allowed in test settings)
        "#p": [user_pubkey.to_hex()]
    });
    
    // Create subscription event with proper tags
    let sub_event = create_subscription_event(
        Kind::Custom(3081),
        filter.clone(),
        &user_keys,
        &service_pubkey,
        &app_name,
    );
    
    // Debug: Check service keys
    println!("Service pubkey from settings: {:?}", app_state.service_keys.as_ref().map(|k| k.public_key().to_hex()));
    println!("Expected service pubkey: {}", service_pubkey.to_hex());
    
    // Process subscription
    let result = process_event(app_state.clone(), sub_event.clone()).await;
    match &result {
        Ok(_) => println!("Subscription processing succeeded"),
        Err(e) => println!("Subscription processing failed: {:?}", e),
    }
    
    // Debug: Print event details
    println!("Event kind: {}", sub_event.kind);
    println!("Event pubkey: {}", sub_event.pubkey.to_hex());
    println!("Event p-tags: {:?}", sub_event.tags.iter()
        .filter(|t| t.kind() == nostr_sdk::TagKind::p())
        .map(|t| t.content())
        .collect::<Vec<_>>());
    
    assert!(result.is_ok(), "Subscription should succeed");
    
    // Verify subscription was stored using the new hash-based storage
    println!("Checking subscriptions for app: {}, pubkey: {}", app_name, user_pubkey.to_hex());
    
    let subscriptions = redis_store::get_subscriptions_by_hash(
        &app_state.redis_pool,
        &app_name,
        &user_pubkey
    )
    .await
    .expect("Failed to get subscriptions");
    
    println!("Found {} subscriptions", subscriptions.len());
    
    assert!(!subscriptions.is_empty(), "Subscription should be stored");
    
    // Verify the filter content matches what we sent
    assert_eq!(subscriptions.len(), 1, "Should have exactly one subscription");
    let (stored_filter, _timestamp) = &subscriptions[0];
    assert_eq!(stored_filter.get("kinds"), filter.get("kinds"), "Filter kinds should match");
    
    // Test unsubscribe
    let unsub_event = create_subscription_event(
        Kind::Custom(3082),
        filter,
        &user_keys,
        &service_pubkey,
        &app_name,
    );
    
    process_event(app_state.clone(), unsub_event).await.expect("Unsubscribe failed");
    
    // Verify subscription was removed
    let subscriptions = redis_store::get_subscriptions_by_hash(
        &app_state.redis_pool,
        &app_name,
        &user_pubkey
    )
    .await
    .expect("Failed to get subscriptions after unsubscribe");
    
    assert!(subscriptions.is_empty(), "Subscription should be removed after unsubscribe");
}

#[tokio::test]
async fn test_encrypted_deregistration() {
    // Setup
    let (service_private_hex, service_pubkey) = generate_service_keypair();
    let (user_sk, user_pubkey) = generate_user_keypair();
    let user_keys = Keys::new(user_sk.clone());
    
    // Create test settings
    let settings = create_test_settings(service_private_hex.clone());
    
    // Use the configured app name
    let app_name = "nostrpushdemo";
    
    // Create app state with mock FCM and cleanup existing data
    let app_state = create_test_app_state(settings, Some(&app_name)).await;
    
    // First register a token
    let token = "dereg_test_token";
    let encrypted_reg = encrypt_token(token, &user_sk, &service_pubkey).await;
    let reg_event = create_test_event(
        Kind::Custom(3079),
        encrypted_reg,
        &user_keys,
        &service_pubkey,
        &app_name,
    );
    process_event(app_state.clone(), reg_event).await.expect("Registration failed");
    
    // Verify token exists
    let tokens = redis_store::get_tokens_for_pubkey_with_app(
        &app_state.redis_pool,
        &app_name,
        &user_pubkey
    )
    .await
    .expect("Failed to get tokens");
    assert_eq!(tokens.len(), 1);
    
    // Now deregister with encrypted token
    let encrypted_dereg = encrypt_token(token, &user_sk, &service_pubkey).await;
    let dereg_event = create_test_event(
        Kind::Custom(3080),
        encrypted_dereg,
        &user_keys,
        &service_pubkey,
        &app_name,
    );
    process_event(app_state.clone(), dereg_event).await.expect("Deregistration failed");
    
    // Verify token was removed
    let tokens = redis_store::get_tokens_for_pubkey_with_app(
        &app_state.redis_pool,
        &app_name,
        &user_pubkey
    )
    .await
    .expect("Failed to get tokens after deregistration");
    assert_eq!(tokens.len(), 0, "Token should be removed after deregistration");
}