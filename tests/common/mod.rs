// Common test utilities for parallel test execution

use anyhow::Result;
use nostr_push_service::redis_store::RedisPool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::env;
use once_cell::sync::Lazy;
use std::sync::Mutex;
use std::future::Future;
use std::time::{Duration, Instant};
use uuid::Uuid;

// Global counter for unique test IDs
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

// Track if we've cleaned Redis once at startup
#[allow(dead_code)]
static REDIS_CLEANED: Lazy<Mutex<bool>> = Lazy::new(|| Mutex::new(false));

/// Namespace for test isolation in Redis
#[allow(dead_code)]
pub struct TestNamespace(String);

impl TestNamespace {
    #[allow(dead_code)]
    pub fn new() -> Self {
        // Use hash-tag to keep related keys co-located in Redis Cluster
        let uuid = Uuid::new_v4();
        Self(format!("test:{{{}}}", uuid))
    }
    
    #[allow(dead_code)]
    pub fn key(&self, k: &str) -> String {
        format!("{}:{}", self.0, k)
    }
    
    #[allow(dead_code)]
    pub fn pattern(&self) -> String {
        format!("{}:*", self.0)
    }
}

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

/// Wipe a specific namespace in Redis using SCAN + UNLINK
#[allow(dead_code)]
pub async fn wipe_namespace(pool: &RedisPool, ns: &TestNamespace) -> Result<()> {
    let mut conn = pool.get().await?;
    let mut cursor: u64 = 0;
    
    loop {
        let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(ns.pattern())
            .arg("COUNT")
            .arg(1000)
            .query_async(&mut *conn)
            .await?;
            
        if !keys.is_empty() {
            let mut pipe = redis::pipe();
            for key in keys {
                pipe.cmd("UNLINK").arg(key);
            }
            pipe.query_async::<()>(&mut *conn).await?;
        }
        
        if next_cursor == 0 {
            break;
        }
        cursor = next_cursor;
    }
    
    Ok(())
}

/// Clean global Redis structures that can cause conflicts between tests
/// This is now deprecated in favor of namespace-based isolation
#[allow(dead_code)]
pub async fn clean_redis_globals(pool: &RedisPool) -> Result<()> {
    // For backward compatibility, still do a FLUSHDB
    // But new tests should use namespace isolation instead
    let mut conn = pool.get().await?;
    redis::cmd("FLUSHDB")
        .query_async::<()>(&mut *conn)
        .await?;
    Ok(())
}

/// No-op function to replace per-test cleanup
#[allow(dead_code)]
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
#[allow(dead_code)]
pub fn generate_test_pubkey_hex() -> String {
    let id = get_unique_test_id();
    // Generate a valid 32-byte hex string (64 chars)
    format!("{:064x}", id)
}

/// Wait for a condition to become true, polling at intervals
#[allow(dead_code)]
pub async fn wait_for_condition<F, Fut>(
    mut check: F,
    poll_interval: Duration,
    timeout: Duration,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let start = Instant::now();
    
    loop {
        if check().await {
            return Ok(());
        }
        
        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!("Timeout waiting for condition after {:?}", timeout));
        }
        
        tokio::time::sleep(poll_interval).await;
    }
}

/// Wait for a value to be available, polling at intervals
#[allow(dead_code)]
pub async fn wait_for_value<F, Fut, T>(
    mut get_value: F,
    poll_interval: Duration,
    timeout: Duration,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let start = Instant::now();
    
    loop {
        if let Some(value) = get_value().await {
            return Ok(value);
        }
        
        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!("Timeout waiting for value after {:?}", timeout));
        }
        
        tokio::time::sleep(poll_interval).await;
    }
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