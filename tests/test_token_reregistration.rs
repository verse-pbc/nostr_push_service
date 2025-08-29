use nostr_push_service::redis_store;
use nostr_sdk::Keys;

#[tokio::test]
async fn test_token_reregistration_by_different_pubkey() {
    // Skip test if Redis is not available
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
    
    let pool = match redis_store::create_pool(&redis_url, 5).await {
        Ok(pool) => pool,
        Err(_) => {
            println!("Skipping test: Redis not available");
            return;
        }
    };

    // Create test data
    let app = "test-app";
    let token = "test_fcm_token_12345";
    
    // Create two different users
    let user1_keys = Keys::generate();
    let user2_keys = Keys::generate();
    let user1_pubkey = user1_keys.public_key();
    let user2_pubkey = user2_keys.public_key();
    
    // Clean up any existing data
    let _ = redis_store::remove_token_with_app(&pool, app, &user1_pubkey, token).await;
    let _ = redis_store::remove_token_with_app(&pool, app, &user2_pubkey, token).await;
    
    // User 1 registers the token
    let result = redis_store::add_or_update_token_with_app(&pool, app, &user1_pubkey, token).await;
    assert!(result.is_ok(), "User 1 should be able to register token");
    
    // Verify user 1 has the token
    let user1_tokens = redis_store::get_tokens_for_pubkey_with_app(&pool, app, &user1_pubkey).await.unwrap();
    assert!(user1_tokens.contains(&token.to_string()), "User 1 should have the token");
    
    // User 2 registers the same token (this should now succeed and transfer ownership)
    let result = redis_store::add_or_update_token_with_app(&pool, app, &user2_pubkey, token).await;
    assert!(result.is_ok(), "User 2 should be able to register the same token");
    
    // Verify user 2 now has the token
    let user2_tokens = redis_store::get_tokens_for_pubkey_with_app(&pool, app, &user2_pubkey).await.unwrap();
    assert!(user2_tokens.contains(&token.to_string()), "User 2 should now have the token");
    
    // Verify user 1 no longer has the token
    let user1_tokens = redis_store::get_tokens_for_pubkey_with_app(&pool, app, &user1_pubkey).await.unwrap();
    assert!(!user1_tokens.contains(&token.to_string()), "User 1 should no longer have the token");
    
    // Clean up
    let _ = redis_store::remove_token_with_app(&pool, app, &user2_pubkey, token).await;
    
    println!("✅ Token re-registration test passed!");
}