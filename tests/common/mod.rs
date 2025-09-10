use nostr_push_service::{
    handlers::CommunityHandler,
    redis_store::RedisPool,
    subscriptions::SubscriptionManager,
};
use redis::AsyncCommands;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Helper to create default subscription manager and community handler for tests
pub fn create_default_handlers() -> (Arc<SubscriptionManager>, Arc<CommunityHandler>) {
    (
        Arc::new(SubscriptionManager::new()),
        Arc::new(CommunityHandler::new()),
    )
}

/// Create a test Redis URL (uses local Redis)
pub fn create_test_redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
}

/// Clean up global Redis structures for test isolation
pub async fn clean_redis_globals(pool: &RedisPool) -> anyhow::Result<()> {
    let mut conn = pool.get().await?;
    
    // Delete all test-related keys
    let keys: Vec<String> = conn.keys("*").await?;
    for key in keys {
        let _: () = conn.del(&key).await?;
    }
    
    Ok(())
}

/// Generate a unique test pubkey hex
pub fn generate_test_pubkey_hex() -> String {
    let counter = get_unique_test_id();
    format!("{:064x}", counter)
}

/// Generate a unique test token
pub fn generate_test_token(prefix: &str) -> String {
    let counter = get_unique_test_id();
    format!("{}_{}", prefix, counter)
}

/// Get a unique test ID
pub fn get_unique_test_id() -> u64 {
    TEST_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Wait for a condition to become true
pub async fn wait_for_condition<F, Fut>(
    mut condition: F,
    timeout: Duration,
    check_interval: Duration,
) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = SystemTime::now();
    loop {
        if condition().await {
            return Ok(());
        }
        
        if SystemTime::now().duration_since(start)? > timeout {
            return Err(anyhow::anyhow!("Timeout waiting for condition"));
        }
        
        tokio::time::sleep(check_interval).await;
    }
}