use anyhow::Result;
use nostr_sdk::{Keys, Kind};

mod common;
use nostr_push_service::{
    config::Settings,
    redis_store::{self},
};

/// Test that subscriptions can be recovered from Redis on startup
#[tokio::test]
async fn test_subscription_recovery_from_redis() -> Result<()> {
    dotenvy::dotenv().ok();
    
    let redis_url = common::create_test_redis_url();
    
    // Safety check
    if let Ok(parsed_url) = url::Url::parse(&redis_url) {
        if parsed_url
            .host_str()
            .is_some_and(|host| host.ends_with(".db.ondigitalocean.com"))
        {
            panic!("Safety check: Cannot run tests against production database");
        }
    }
    
    let settings = Settings::new()?;
    let redis_pool = redis_store::create_pool(&redis_url, settings.redis.connection_pool_size).await?;
    
    // Create test users and subscriptions
    let user1_keys = Keys::generate();
    let user2_keys = Keys::generate();
    let app = "peek";
    
    // Create filters
    let filter1 = nostr_sdk::Filter::new().kinds(vec![Kind::Custom(9)]);
    let filter2 = nostr_sdk::Filter::new().kinds(vec![Kind::Custom(1111)]);
    
    let filter1_value = serde_json::to_value(&filter1)?;
    let filter2_value = serde_json::to_value(&filter2)?;
    
    let timestamp = nostr_sdk::Timestamp::now().as_u64();
    
    // Add subscriptions using the hash-based storage
    redis_store::add_subscription_by_filter(
        &redis_pool,
        app,
        &user1_keys.public_key(),
        &filter1_value,
        timestamp,
    ).await?;
    
    redis_store::add_subscription_by_filter(
        &redis_pool,
        app,
        &user2_keys.public_key(),
        &filter2_value,
        timestamp,
    ).await?;
    
    // Add another subscription for user1
    redis_store::add_subscription_by_filter(
        &redis_pool,
        app,
        &user1_keys.public_key(),
        &filter2_value,
        timestamp,
    ).await?;
    
    // Now call get_all_active_subscriptions to simulate startup recovery
    let recovered = redis_store::get_all_active_subscriptions(&redis_pool).await?;
    
    // Should have 3 subscriptions total:
    // - user1: filter1
    // - user1: filter2
    // - user2: filter2
    assert_eq!(
        recovered.len(),
        3,
        "Should recover all 3 subscriptions from Redis"
    );
    
    // Verify the recovered subscriptions contain the correct filters
    let mut recovered_filter_kinds = std::collections::HashSet::new();
    for (_key, filter_value) in recovered.iter() {
        if let Ok(filter) = serde_json::from_value::<nostr_sdk::Filter>(filter_value.clone()) {
            // Extract kinds from the filter
            let filter_json = serde_json::to_value(&filter)?;
            if let Some(kinds) = filter_json.get("kinds").and_then(|k| k.as_array()) {
                for kind in kinds {
                    if let Some(k) = kind.as_u64() {
                        recovered_filter_kinds.insert(k);
                    }
                }
            }
        }
    }
    
    // Should have both kind 9 and kind 1111
    assert!(
        recovered_filter_kinds.contains(&9),
        "Should recover kind 9 subscription"
    );
    assert!(
        recovered_filter_kinds.contains(&1111),
        "Should recover kind 1111 subscription"
    );
    
    println!("✅ Successfully recovered {} subscriptions with kinds: {:?}", 
             recovered.len(), recovered_filter_kinds);
    
    Ok(())
}

#[tokio::test]
async fn test_subscription_recovery_with_multiple_apps() -> Result<()> {
    dotenvy::dotenv().ok();
    
    let redis_url = common::create_test_redis_url();
    let settings = Settings::new()?;
    let redis_pool = redis_store::create_pool(&redis_url, settings.redis.connection_pool_size).await?;
    
    let user_keys = Keys::generate();
    let filter = nostr_sdk::Filter::new().kinds(vec![Kind::Custom(9)]);
    let filter_value = serde_json::to_value(&filter)?;
    let timestamp = nostr_sdk::Timestamp::now().as_u64();
    
    // Add same subscription for multiple apps
    redis_store::add_subscription_by_filter(
        &redis_pool,
        "peek",
        &user_keys.public_key(),
        &filter_value,
        timestamp,
    ).await?;
    
    redis_store::add_subscription_by_filter(
        &redis_pool,
        "universes",
        &user_keys.public_key(),
        &filter_value,
        timestamp,
    ).await?;
    
    // Recover all subscriptions
    let recovered = redis_store::get_all_active_subscriptions(&redis_pool).await?;
    
    // Should have 2 subscriptions (one per app)
    assert_eq!(
        recovered.len(),
        2,
        "Should recover subscriptions from both apps"
    );
    
    // Verify keys contain the app names
    let keys: Vec<String> = recovered.keys().cloned().collect();
    let has_peek = keys.iter().any(|k| k.contains("peek"));
    let has_universes = keys.iter().any(|k| k.contains("universes"));
    
    assert!(has_peek, "Should recover peek subscription");
    assert!(has_universes, "Should recover universes subscription");
    
    println!("✅ Successfully recovered subscriptions from multiple apps");
    
    Ok(())
}
