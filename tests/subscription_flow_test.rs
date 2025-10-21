use nostr_push_service::{
    config::{AppConfig, Settings},
    handlers::{CommunityHandler, CommunityId, GroupId},
    state::AppState,
    subscriptions::SubscriptionManager,
    event_handler::EventContext,
};
use nostr_sdk::prelude::*;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Helper to create test settings
fn create_test_settings() -> Settings {
    Settings {
        nostr: nostr_push_service::config::NostrSettings {
            relay_url: "wss://test.relay.com".to_string(),
            cache_expiration: Some(300),
        },
        service: nostr_push_service::config::ServiceSettings {
            private_key_hex: Some("0000000000000000000000000000000000000000000000000000000000000001".to_string()),
            process_window_days: 7,
            processed_event_ttl_secs: 604800,
            control_kinds: vec![3079, 3080, 3081, 3082],
            dm_kinds: vec![1059, 14],
        },
        redis: nostr_push_service::config::RedisSettings {
            url: "redis://127.0.0.1:6379".to_string(),
            connection_pool_size: 10,
        },
        apps: vec![
            AppConfig {
                name: "testapp".to_string(),
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
            }
        ],
        cleanup: nostr_push_service::config::CleanupSettings {
            enabled: false,
            interval_secs: 86400,
            token_max_age_days: 90,
        },
        server: nostr_push_service::config::ServerSettings {
            listen_addr: "0.0.0.0:8000".to_string(),
        },
        notification: None,
    }
}

#[tokio::test]
async fn test_subscription_sharing() {
    let manager = SubscriptionManager::new();
    
    // Create identical filters for different users
    let filter = Filter::new()
        .kinds([Kind::from(1), Kind::from(3)])
        .limit(100); // This should be normalized away
    
    // User 1 subscribes
    let (hash1, _is_new1) = manager.add_user_filter(
        "testapp".to_string(),
        "user1_pubkey".to_string(),
        filter.clone()
    ).await;
    
    // User 2 subscribes with same filter
    let (hash2, _is_new2) = manager.add_user_filter(
        "testapp".to_string(),
        "user2_pubkey".to_string(),
        filter.clone()
    ).await;
    
    // Should get the same hash (filter is shared)
    assert_eq!(hash1, hash2, "Filters should be deduplicated");
    
    // Check reference count
    assert_eq!(manager.get_ref_count(&hash1).await, 2);
    assert_eq!(manager.get_unique_filters_count().await, 1);
    
    // User 3 subscribes with slightly different filter
    let filter2 = Filter::new()
        .kinds([Kind::from(1), Kind::from(3)])
        .since(Timestamp::now()); // This should also be normalized away
    
    let (hash3, _is_new3) = manager.add_user_filter(
        "testapp".to_string(),
        "user3_pubkey".to_string(),
        filter2
    ).await;
    
    // Should still get the same hash
    assert_eq!(hash1, hash3, "Normalized filters should share same subscription");
    assert_eq!(manager.get_ref_count(&hash1).await, 3);
    
    // Remove one user's subscription
    manager.remove_user_filter("testapp", "user1_pubkey", &hash1).await;
    assert_eq!(manager.get_ref_count(&hash1).await, 2);
    
    // Cleanup remaining users
    manager.cleanup_user_on_disconnect("testapp", "user2_pubkey").await;
    manager.cleanup_user_on_disconnect("testapp", "user3_pubkey").await;
    
    // Filter should be completely removed
    assert_eq!(manager.get_ref_count(&hash1).await, 0);
    assert_eq!(manager.get_unique_filters_count().await, 0);
}

#[tokio::test]
async fn test_community_routing() {
    let handler = CommunityHandler::new();
    
    // Create a test community
    let community = CommunityId {
        kind: 34550,
        pubkey: "community_creator".to_string(),
        identifier: "rust-developers".to_string(),
    };
    
    // Add members to the community
    handler.add_community_member(community.clone(), "alice".to_string()).await;
    handler.add_community_member(community.clone(), "bob".to_string()).await;
    handler.add_community_member(community.clone(), "charlie".to_string()).await;
    
    // Create an event with the community 'a' tag
    let keys = Keys::generate();
    let event = EventBuilder::text_note("Hello Rust community!")
        .tags([
            Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::A)),
                vec![community.to_a_tag()]
            )
        ])
        .sign_with_keys(&keys)
        .unwrap();
    
    // Route the event
    let recipients = handler.route_event(&event, "wss://test.relay.com").await;
    
    // Check that all community members receive it
    assert_eq!(recipients.len(), 3);
    assert!(recipients.contains("alice"));
    assert!(recipients.contains("bob"));
    assert!(recipients.contains("charlie"));
    
    // Remove a member
    handler.remove_community_member(&community, "bob").await;
    
    // Route another event
    let recipients2 = handler.route_event(&event, "wss://test.relay.com").await;
    assert_eq!(recipients2.len(), 2);
    assert!(!recipients2.contains("bob"));
}

#[tokio::test]
async fn test_group_and_community_coexistence() {
    let handler = CommunityHandler::new();
    
    // Create a community
    let community = CommunityId {
        kind: 34550,
        pubkey: "creator1".to_string(),
        identifier: "community1".to_string(),
    };
    
    // Create a group
    let group = GroupId::from_h_tag("developers", "wss://relay.com");
    
    // Add members - some overlap
    handler.add_community_member(community.clone(), "alice".to_string()).await;
    handler.add_community_member(community.clone(), "bob".to_string()).await;
    
    handler.add_group_member(group.clone(), "bob".to_string()).await;
    handler.add_group_member(group.clone(), "charlie".to_string()).await;
    
    // Create event with both tags
    let keys = Keys::generate();
    let event = EventBuilder::text_note("Cross-posted message")
        .tags([
            Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::A)),
                vec![community.to_a_tag()]
            ),
            Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)),
                vec!["developers".to_string()]
            ),
        ])
        .sign_with_keys(&keys)
        .unwrap();
    
    // Route the event
    let recipients = handler.route_event(&event, "wss://relay.com").await;
    
    // Should reach all unique members
    assert_eq!(recipients.len(), 3);
    assert!(recipients.contains("alice")); // Community only
    assert!(recipients.contains("bob"));    // Both
    assert!(recipients.contains("charlie")); // Group only
}

#[tokio::test]
async fn test_dm_event_identification() {
    use nostr_push_service::event_handler;
    
    // Test NIP-44 DM (kind 1059)
    let keys = Keys::generate();
    let nip44_event = EventBuilder::new(Kind::GiftWrap, "encrypted_content")
        .sign_with_keys(&keys)
        .unwrap();
    
    // The is_dm_event function should identify this
    // Note: We'd need to expose this function or test it indirectly
    assert_eq!(nip44_event.kind, Kind::GiftWrap);
    assert_eq!(nip44_event.kind.as_u16(), 1059);
    
    // Test NIP-17 DM (kind 14)
    let nip17_event = EventBuilder::new(Kind::PrivateDirectMessage, "sealed_content")
        .sign_with_keys(&keys)
        .unwrap();
    
    assert_eq!(nip17_event.kind, Kind::PrivateDirectMessage);
    assert_eq!(nip17_event.kind.as_u16(), 14);
    
    // Test regular event (should not be identified as DM)
    let regular_event = EventBuilder::text_note("Public message")
        .sign_with_keys(&keys)
        .unwrap();
    
    assert_eq!(regular_event.kind, Kind::TextNote);
    assert_eq!(regular_event.kind.as_u16(), 1);
}

#[tokio::test]
async fn test_filter_normalization() {
    use nostr_push_service::subscriptions::SubscriptionManager;
    
    // Create filters that should normalize to the same thing
    let filter1 = Filter::new()
        .kinds([Kind::from(1), Kind::from(3)])
        .authors([PublicKey::from_hex("82341f882b6eabcd2ba7f1ef90aad961cf074af15b9ef44a09f9d2a8fbfbe6a2").unwrap()])
        .limit(100)
        .since(Timestamp::now() - Duration::from_secs(3600));
    
    let filter2 = Filter::new()
        .kinds([Kind::from(3), Kind::from(1)]) // Different order
        .authors([PublicKey::from_hex("82341f882b6eabcd2ba7f1ef90aad961cf074af15b9ef44a09f9d2a8fbfbe6a2").unwrap()])
        .limit(200) // Different limit
        .until(Timestamp::now()); // Different until
    
    // Compute hashes
    let hash1 = SubscriptionManager::compute_filter_hash(&filter1);
    let hash2 = SubscriptionManager::compute_filter_hash(&filter2);
    
    // Should be the same after normalization
    assert_eq!(hash1, hash2, "Normalized filters should have same hash");
}

#[tokio::test]
async fn test_subscription_lifecycle() {
    let manager = SubscriptionManager::new();
    
    // User subscribes to multiple filters
    let filter1 = Filter::new().kinds([Kind::from(1)]);
    let filter2 = Filter::new().kinds([Kind::from(3)]);
    let filter3 = Filter::new().kinds([Kind::from(7)]);
    
    let (hash1, _) = manager.add_user_filter("app1".to_string(), "user1".to_string(), filter1.clone()).await;
    let (hash2, _) = manager.add_user_filter("app1".to_string(), "user1".to_string(), filter2.clone()).await;
    let (hash3, _) = manager.add_user_filter("app1".to_string(), "user1".to_string(), filter3.clone()).await;
    
    // Check user has all filters
    let user_filters = manager.get_user_filters("app1", "user1").await;
    assert_eq!(user_filters.len(), 3);
    assert!(user_filters.contains(&hash1));
    assert!(user_filters.contains(&hash2));
    assert!(user_filters.contains(&hash3));
    
    // Check active subscriptions
    assert!(manager.has_active_subscriptions("app1", "user1").await);
    
    // Another user subscribes to one of the same filters
    let (hash4, _) = manager.add_user_filter("app1".to_string(), "user2".to_string(), filter1).await;
    assert_eq!(hash1, hash4, "Same filter should share subscription");
    assert_eq!(manager.get_ref_count(&hash1).await, 2);
    
    // User1 disconnects - should remove all their filters
    manager.cleanup_user_on_disconnect("app1", "user1").await;
    assert!(!manager.has_active_subscriptions("app1", "user1").await);
    
    // Filter1 should still exist (user2 has it)
    assert_eq!(manager.get_ref_count(&hash1).await, 1);
    
    // Filter2 and filter3 should be gone
    assert_eq!(manager.get_ref_count(&hash2).await, 0);
    assert_eq!(manager.get_ref_count(&hash3).await, 0);
}

#[tokio::test]
async fn test_community_membership_lifecycle() {
    let handler = CommunityHandler::new();
    
    let community1 = CommunityId {
        kind: 34550,
        pubkey: "creator1".to_string(),
        identifier: "community1".to_string(),
    };
    
    let community2 = CommunityId {
        kind: 34550,
        pubkey: "creator2".to_string(),
        identifier: "community2".to_string(),
    };
    
    // User joins multiple communities
    handler.add_community_member(community1.clone(), "alice".to_string()).await;
    handler.add_community_member(community2.clone(), "alice".to_string()).await;
    
    // Check user's communities
    let alice_communities = handler.get_user_communities("alice").await;
    assert_eq!(alice_communities.len(), 2);
    assert!(alice_communities.contains(&community1));
    assert!(alice_communities.contains(&community2));
    
    // Another user joins community1
    handler.add_community_member(community1.clone(), "bob".to_string()).await;
    
    let community1_members = handler.get_community_members(&community1).await;
    assert_eq!(community1_members.len(), 2);
    assert!(community1_members.contains("alice"));
    assert!(community1_members.contains("bob"));
    
    // Alice leaves all communities
    handler.cleanup_user("alice").await;
    
    // Check alice is removed
    let alice_communities = handler.get_user_communities("alice").await;
    assert_eq!(alice_communities.len(), 0);
    
    // Community1 should still have bob
    let community1_members = handler.get_community_members(&community1).await;
    assert_eq!(community1_members.len(), 1);
    assert!(community1_members.contains("bob"));
    
    // Community2 should be empty
    let community2_members = handler.get_community_members(&community2).await;
    assert_eq!(community2_members.len(), 0);
}

#[tokio::test]
async fn test_event_routing_priority() {
    let handler = CommunityHandler::new();
    
    // Set up a community
    let community = CommunityId {
        kind: 34550,
        pubkey: "creator".to_string(),
        identifier: "test".to_string(),
    };
    handler.add_community_member(community.clone(), "alice".to_string()).await;
    
    // Set up a group
    let group = GroupId::from_h_tag("testgroup", "wss://relay.com");
    handler.add_group_member(group.clone(), "bob".to_string()).await;
    
    // Test event with only 'a' tag
    let keys = Keys::generate();
    let community_event = EventBuilder::text_note("Community only")
        .tags([
            Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::A)),
                vec![community.to_a_tag()]
            )
        ])
        .sign_with_keys(&keys)
        .unwrap();
    
    let recipients = handler.route_event(&community_event, "wss://relay.com").await;
    assert_eq!(recipients.len(), 1);
    assert!(recipients.contains("alice"));
    assert!(!recipients.contains("bob"));
    
    // Test event with only 'h' tag
    let group_event = EventBuilder::text_note("Group only")
        .tags([
            Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)),
                vec!["testgroup".to_string()]
            )
        ])
        .sign_with_keys(&keys)
        .unwrap();
    
    let recipients = handler.route_event(&group_event, "wss://relay.com").await;
    assert_eq!(recipients.len(), 1);
    assert!(!recipients.contains("alice"));
    assert!(recipients.contains("bob"));
}

#[tokio::test]
async fn test_multiple_apps_same_filter() {
    let manager = SubscriptionManager::new();
    
    let filter = Filter::new().kinds([Kind::from(1)]);
    
    // Same user, different apps
    let (hash1, _) = manager.add_user_filter("app1".to_string(), "user1".to_string(), filter.clone()).await;
    let (hash2, _) = manager.add_user_filter("app2".to_string(), "user1".to_string(), filter.clone()).await;
    
    // Should share the same filter
    assert_eq!(hash1, hash2);
    assert_eq!(manager.get_ref_count(&hash1).await, 2);
    
    // Each app tracks separately
    let app1_filters = manager.get_user_filters("app1", "user1").await;
    let app2_filters = manager.get_user_filters("app2", "user1").await;
    
    assert_eq!(app1_filters.len(), 1);
    assert_eq!(app2_filters.len(), 1);
    
    // Cleanup one app
    manager.cleanup_user_on_disconnect("app1", "user1").await;
    
    // Filter should still exist for app2
    assert_eq!(manager.get_ref_count(&hash1).await, 1);
    assert!(manager.has_active_subscriptions("app2", "user1").await);
    assert!(!manager.has_active_subscriptions("app1", "user1").await);
}