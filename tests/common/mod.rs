// Common test utilities for parallel test execution

use anyhow::Result;
use nostr_push_service::redis_store::RedisPool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::env;
use once_cell::sync::Lazy;
use std::sync::Mutex;

// Global counter for unique test IDs
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

// Track if we've cleaned Redis once at startup
static REDIS_CLEANED: Lazy<Mutex<bool>> = Lazy::new(|| Mutex::new(false));

/// Get a unique test ID for data isolation
pub fn get_unique_test_id() -> u64 {
    TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Create a standard Redis URL (no database selection)
pub fn create_test_redis_url() -> String {
    let redis_host = env::var("REDIS_HOST").unwrap_or_else(|_| "localhost".to_string());
    let redis_port = env::var("REDIS_PORT").unwrap_or_else(|_| "6379".to_string());
    format!("redis://{}:{}", redis_host, redis_port)
}

/// Clean Redis once at the start of test run (optional)
pub async fn clean_redis_once(pool: &RedisPool) -> Result<()> {
    let mut cleaned = REDIS_CLEANED.lock().unwrap();
    if !*cleaned {
        println!("Performing one-time Redis cleanup for test run...");
        let mut conn = pool.get().await?;
        redis::cmd("FLUSHDB")
            .query_async::<()>(&mut *conn)
            .await?;
        *cleaned = true;
        println!("Redis cleanup complete.");
    }
    Ok(())
}

/// No-op function to replace per-test cleanup
pub async fn setup_test_db(_pool: &RedisPool) -> Result<()> {
    // No longer flush per test - we use unique test data instead
    Ok(())
}

/// Generate unique test token
pub fn generate_test_token(prefix: &str) -> String {
    let id = get_unique_test_id();
    format!("test_token_{}_{}", prefix, id)
}

/// Generate unique test pubkey hex
pub fn generate_test_pubkey_hex() -> String {
    let id = get_unique_test_id();
    // Generate a valid 32-byte hex string (64 chars)
    format!("{:064x}", id)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_unique_test_ids() {
        let id1 = get_unique_test_id();
        let id2 = get_unique_test_id();
        assert_ne!(id1, id2, "Test IDs should be unique");
    }
    
    #[test]
    fn test_unique_tokens() {
        let token1 = generate_test_token("test");
        let token2 = generate_test_token("test");
        assert_ne!(token1, token2, "Tokens should be unique");
    }
}