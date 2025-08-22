// Common test utilities for parallel test execution

use anyhow::Result;
use nostr_push_service::redis_store::RedisPool;
use std::sync::atomic::{AtomicU8, Ordering};
use std::env;

// Redis supports databases 0-15, we'll use 1-15 for tests (0 for production)
// Use Relaxed ordering for better performance in parallel tests
static REDIS_DB_COUNTER: AtomicU8 = AtomicU8::new(1);

/// Get a unique Redis database number for test isolation
pub fn get_test_redis_db() -> u8 {
    // Use modulo to cycle through databases 1-15
    let counter = REDIS_DB_COUNTER.fetch_add(1, Ordering::Relaxed);
    // Map to databases 1-15 (avoiding 0 which might be used for production)
    (counter % 15) + 1
}

/// Create a Redis URL with a specific database number
pub fn create_test_redis_url(db_num: u8) -> String {
    let redis_host = env::var("REDIS_HOST").unwrap_or_else(|_| "localhost".to_string());
    let redis_port = env::var("REDIS_PORT").unwrap_or_else(|_| "6379".to_string());
    format!("redis://{}:{}/{}", redis_host, redis_port, db_num)
}

/// Setup a clean test database - flushes only the specific database
pub async fn setup_test_db(pool: &RedisPool) -> Result<()> {
    // Get a connection with a longer timeout for the flush operation
    let mut conn = pool.get_timeout(std::time::Duration::from_secs(30)).await
        .map_err(|e| anyhow::anyhow!("Failed to get Redis connection for cleanup: {}", e))?;
    
    // FLUSHDB only flushes the current database, not all databases
    redis::cmd("FLUSHDB")
        .query_async::<()>(&mut *conn)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to flush test database: {}", e))?;
    Ok(())
}

/// Test data builder for creating consistent test data
pub struct TestDataBuilder {
    pub test_pubkey: String,
    pub test_token: String,
    pub test_event_id: String,
}

impl TestDataBuilder {
    pub fn new(test_id: u64) -> Self {
        Self {
            test_pubkey: format!("test_pubkey_{}", test_id),
            test_token: format!("test_token_{}", test_id),
            test_event_id: format!("test_event_{}", test_id),
        }
    }
    
    pub fn with_suffix(&self, suffix: &str) -> Self {
        Self {
            test_pubkey: format!("{}_{}", self.test_pubkey, suffix),
            test_token: format!("{}_{}", self.test_token, suffix),
            test_event_id: format!("{}_{}", self.test_event_id, suffix),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_unique_db_numbers() {
        let db1 = get_test_redis_db();
        let db2 = get_test_redis_db();
        assert_ne!(db1, db2, "Database numbers should be unique");
        assert!(db1 >= 1 && db1 <= 15, "DB number should be between 1-15");
        assert!(db2 >= 1 && db2 <= 15, "DB number should be between 1-15");
    }
    
    #[test]
    fn test_redis_url_with_db() {
        env::set_var("REDIS_HOST", "testhost");
        env::set_var("REDIS_PORT", "6380");
        let url = create_test_redis_url(5);
        assert_eq!(url, "redis://testhost:6380/5");
    }
}