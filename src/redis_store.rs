use crate::crypto::hash_filter;
use crate::error::{Result, ServiceError};
use bb8_redis::bb8::Pool;
use bb8_redis::RedisConnectionManager;
use nostr_sdk::{EventId, PublicKey, Timestamp};
use redis::{RedisResult, Value};
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::time::Duration;
use tracing::warn;

// Type alias for the connection pool
pub type RedisPool = Pool<RedisConnectionManager>;

// Constants for Redis keys and sets
const PROCESSED_EVENTS_SET: &str = "processed_nostr_events";
const PUBKEY_DEVICE_TOKENS_SET_PREFIX: &str = "user_tokens:";
const STALE_TOKENS_ZSET: &str = "stale_tokens";
pub const TOKEN_TO_PUBKEY_HASH: &str = "token_to_pubkey";

// Default app namespace for backward compatibility
const DEFAULT_APP: &str = "default";

/// Build a namespaced key for user tokens
fn build_user_tokens_key(app: &str, pubkey: &PublicKey) -> String {
    format!("app:{}:user_tokens:{}", app, pubkey.to_hex())
}

/// Build a namespaced key for token to pubkey mapping
fn build_token_to_pubkey_key(app: &str) -> String {
    format!("app:{}:token_to_pubkey", app)
}

/// Build a namespaced key for subscriptions
fn build_subscriptions_key(app: &str, pubkey: &PublicKey) -> String {
    format!("app:{}:subscriptions:{}", app, pubkey.to_hex())
}

/// Build a namespaced key for stale tokens
fn build_stale_tokens_key(app: &str) -> String {
    format!("app:{}:stale_tokens", app)
}

/// Creates a new Redis connection pool.
pub async fn create_pool(redis_url: &str, pool_size: u32) -> Result<RedisPool> {
    let manager = RedisConnectionManager::new(redis_url).map_err(ServiceError::Redis)?;
    Pool::builder()
        .max_size(pool_size)
        .connection_timeout(Duration::from_secs(15))
        .build(manager)
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to build Redis pool: {}", e)))
}

/// Retrieves device tokens associated with a public key (backward compatible - uses default app)
pub async fn get_tokens_for_pubkey(pool: &RedisPool, pubkey: &PublicKey) -> Result<Vec<String>> {
    get_tokens_for_pubkey_with_app(pool, DEFAULT_APP, pubkey).await
}

/// Retrieves device tokens associated with a public key from ALL app namespaces
/// This is useful for notifications that should be sent regardless of which app registered the token
pub async fn get_all_tokens_for_pubkey(pool: &RedisPool, pubkey: &PublicKey) -> Result<Vec<String>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    // Find all user token keys for this pubkey across all apps
    let pattern = format!("app:*:user_tokens:{}", pubkey.to_hex());
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(&pattern)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    // Collect all unique tokens from all apps
    let mut all_tokens = HashSet::new();
    for key in keys {
        let tokens: Vec<String> = redis::cmd("SMEMBERS")
            .arg(&key)
            .query_async(&mut *conn)
            .await
            .map_err(ServiceError::Redis)?;
        
        for token in tokens {
            all_tokens.insert(token);
        }
    }
    
    Ok(all_tokens.into_iter().collect())
}

/// Retrieves device tokens grouped by app for a public key
pub async fn get_tokens_by_app_for_pubkey(pool: &RedisPool, pubkey: &PublicKey) -> Result<HashMap<String, Vec<String>>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    // Find all user token keys for this pubkey across all apps
    let pattern = format!("app:*:user_tokens:{}", pubkey.to_hex());
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(&pattern)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    let mut tokens_by_app = HashMap::new();
    
    for key in keys {
        // Extract app name from key pattern: app:{app}:user_tokens:{pubkey}
        let parts: Vec<&str> = key.split(':').collect();
        if parts.len() >= 4 {
            let app = parts[1].to_string();
            let tokens: Vec<String> = redis::cmd("SMEMBERS")
                .arg(&key)
                .query_async(&mut *conn)
                .await
                .map_err(ServiceError::Redis)?;
            
            if !tokens.is_empty() {
                tokens_by_app.insert(app, tokens);
            }
        }
    }
    
    Ok(tokens_by_app)
}

/// Retrieves device tokens associated with a public key for a specific app
pub async fn get_tokens_for_pubkey_with_app(
    pool: &RedisPool,
    app: &str,
    pubkey: &PublicKey,
) -> Result<Vec<String>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    let user_tokens_key = build_user_tokens_key(app, pubkey);

    let tokens: Vec<String> = redis::cmd("SMEMBERS")
        .arg(&user_tokens_key)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    Ok(tokens)
}

/// Removes specific device tokens associated with a public key.
/// This function now only removes from the user's token set.
/// Use `remove_token_globally` for complete removal.
pub async fn remove_device_tokens(
    pool: &RedisPool,
    pubkey: &PublicKey,
    tokens: &[String],
) -> Result<usize> {
    if tokens.is_empty() {
        return Ok(0);
    }
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    let user_tokens_key = format!("{}{}", PUBKEY_DEVICE_TOKENS_SET_PREFIX, pubkey.to_hex());

    // Only remove from the user's set here. Global removal handles other structures.
    let count: usize = redis::cmd("SREM")
        .arg(&user_tokens_key)
        .arg(tokens)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;

    Ok(count)
}

/// Checks if an event has already been processed.
pub async fn is_event_processed(pool: &RedisPool, event_id: &EventId) -> Result<bool> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    let processed: bool = redis::cmd("SISMEMBER")
        .arg(PROCESSED_EVENTS_SET)
        .arg(event_id.to_hex())
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    Ok(processed)
}

/// Marks an event as processed with a time-to-live (TTL).
pub async fn mark_event_processed(
    pool: &RedisPool,
    event_id: &EventId,
    ttl_seconds: u64,
) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    let ttl_i64: i64 = ttl_seconds
        .try_into()
        .map_err(|_| ServiceError::Internal("TTL value too large".to_string()))?;

    let mut pipe = redis::pipe();
    pipe.atomic()
        .sadd(PROCESSED_EVENTS_SET, event_id.to_hex())
        .expire(PROCESSED_EVENTS_SET, ttl_i64);

    // Query the pipeline. Expect redis::Value to handle the array result from MULTI/EXEC.
    pipe.query_async::<Value>(&mut *conn)
        .await
        .map(|_| ())
        .map_err(ServiceError::Redis)
}

/// Adds or updates a single device token for a pubkey (backward compatible - uses default app)
pub async fn add_or_update_token(pool: &RedisPool, pubkey: &PublicKey, token: &str) -> Result<()> {
    add_or_update_token_with_app(pool, DEFAULT_APP, pubkey, token).await
}

/// Adds or updates a single device token for a pubkey with app namespace
pub async fn add_or_update_token_with_app(
    pool: &RedisPool,
    app: &str,
    pubkey: &PublicKey,
    token: &str,
) -> Result<()> {
    let now_timestamp = Timestamp::now().as_u64();
    let pubkey_hex = pubkey.to_hex();
    let user_tokens_key = build_user_tokens_key(app, pubkey);
    let token_to_pubkey_key = build_token_to_pubkey_key(app);
    let stale_tokens_key = build_stale_tokens_key(app);

    let mut conn = pool
        .get()
        .await
        .map_err(|e| {
            ServiceError::Internal(format!("Failed to get Redis connection: {}", e))
        })?;

    // Check if this token is already associated with a different pubkey in this app
    let existing_pubkey: Option<String> = redis::cmd("HGET")
        .arg(&token_to_pubkey_key)
        .arg(token)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    // Use a pipeline to ensure atomicity
    let mut pipe = redis::pipe();
    pipe.atomic();
    
    // If token was previously registered to a different pubkey, clean up old association
    if let Some(existing) = existing_pubkey {
        if existing != pubkey_hex {
            warn!(
                "Token re-registration for app {}: Token moving from pubkey {} to {}",
                app, existing, pubkey_hex
            );
            // Parse the existing pubkey and remove token from old user's set
            if let Ok(old_pubkey) = PublicKey::from_hex(&existing) {
                let old_user_tokens_key = build_user_tokens_key(app, &old_pubkey);
                pipe.srem(&old_user_tokens_key, token);
            }
        }
    }
    
    // Add/update token for new pubkey
    pipe.sadd(&user_tokens_key, token) // Add token to user's set
        .zadd(&stale_tokens_key, token, now_timestamp) // Add/update token in sorted set with current timestamp
        .hset(&token_to_pubkey_key, token, &pubkey_hex); // Map token back to pubkey

    let _result = pipe.query_async::<Value>(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    Ok(())
}

/// Removes a single device token for a pubkey (backward compatible - uses default app)
pub async fn remove_token(pool: &RedisPool, pubkey: &PublicKey, token: &str) -> Result<bool> {
    remove_token_with_app(pool, DEFAULT_APP, pubkey, token).await
}

/// Removes a single device token for a pubkey with app namespace.
/// Returns true if the token was found and removed.
/// Only allows removal if the token is registered to the requesting pubkey in this app.
pub async fn remove_token_with_app(
    pool: &RedisPool,
    app: &str,
    pubkey: &PublicKey,
    token: &str,
) -> Result<bool> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    let token_to_pubkey_key = build_token_to_pubkey_key(app);
    
    // First check if this token belongs to the requesting pubkey in this app
    let token_owner: Option<String> = redis::cmd("HGET")
        .arg(&token_to_pubkey_key)
        .arg(token)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    let pubkey_hex = pubkey.to_hex();
    
    match token_owner {
        Some(owner) if owner == pubkey_hex => {
            // Token belongs to this pubkey in this app, proceed with removal
            let user_tokens_key = build_user_tokens_key(app, pubkey);
            let stale_tokens_key = build_stale_tokens_key(app);
            
            let mut pipe = redis::pipe();
            pipe.atomic()
                .srem(&user_tokens_key, token) // Remove from user's set
                .zrem(&stale_tokens_key, token) // Remove from sorted set
                .hdel(&token_to_pubkey_key, token); // Remove from token->pubkey map
            
            let _result: RedisResult<(usize, usize, usize)> = pipe.query_async(&mut *conn).await;
            _result.map(|_| ()).map_err(ServiceError::Redis)?;
            Ok(true)
        }
        Some(owner) => {
            warn!(
                "Deregistration rejected for app {}! Token owned by {} but deregistration attempted by {}",
                app, owner, pubkey_hex
            );
            Ok(false) // Return false to indicate token wasn't removed
        }
        None => {
            // Token doesn't exist in this app
            Ok(false)
        }
    }
}

/// Removes a token globally: from the user's set, the stale token ZSET, and the token->pubkey HASH.
pub async fn remove_token_globally(
    pool: &RedisPool,
    pubkey: &PublicKey,
    token: &str,
) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;

    let user_tokens_key = format!("{}{}", PUBKEY_DEVICE_TOKENS_SET_PREFIX, pubkey.to_hex());

    let mut pipe = redis::pipe();
    pipe.atomic()
        .srem(&user_tokens_key, token) // Remove from user's set
        .zrem(STALE_TOKENS_ZSET, token) // Remove from sorted set
        .hdel(TOKEN_TO_PUBKEY_HASH, token); // Remove from token->pubkey map

    let _result: RedisResult<(usize, usize, usize)> = pipe.query_async(&mut *conn).await;

    _result.map(|_| ()).map_err(ServiceError::Redis)
}

/// Cleans up stale tokens based on their last_seen timestamp stored in the ZSET.
pub async fn cleanup_stale_tokens(pool: &RedisPool, max_age_seconds: i64) -> Result<usize> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;

    let now_timestamp = Timestamp::now().as_u64();
    let cutoff_timestamp = now_timestamp.saturating_sub(max_age_seconds.try_into().unwrap_or(0));

    // 1. Find tokens with score <= cutoff_timestamp
    let stale_tokens: Vec<String> = redis::cmd("ZRANGEBYSCORE")
        .arg(STALE_TOKENS_ZSET)
        .arg("-inf")
        .arg(cutoff_timestamp)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;

    if stale_tokens.is_empty() {
        tracing::debug!("No stale tokens found to clean up.");
        return Ok(0);
    }

    let count = stale_tokens.len();
    tracing::info!("Found {} stale tokens to clean up.", count);

    // 2. Get the associated pubkeys for the stale tokens
    let pubkeys_hex: Vec<Option<String>> = redis::cmd("HMGET")
        .arg(TOKEN_TO_PUBKEY_HASH)
        .arg(&stale_tokens)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;

    // 3. Start a pipeline for atomic removal
    let mut pipe = redis::pipe();
    pipe.atomic();

    // 3a. Remove tokens from the ZSET
    let mut zrem_cmd = redis::cmd("ZREMRANGEBYSCORE");
    zrem_cmd
        .arg(STALE_TOKENS_ZSET)
        .arg("-inf")
        .arg(cutoff_timestamp);
    pipe.add_command(zrem_cmd);

    // 3b. Remove tokens from the HASH
    pipe.hdel(TOKEN_TO_PUBKEY_HASH, &stale_tokens);

    // 3c. Remove tokens from individual user sets
    let mut actual_removed_count = 0;
    for (token, pubkey_hex_opt) in stale_tokens.iter().zip(pubkeys_hex.iter()) {
        if let Some(pubkey_hex) = pubkey_hex_opt {
            let user_tokens_key = format!("{}{}", PUBKEY_DEVICE_TOKENS_SET_PREFIX, pubkey_hex);
            pipe.srem(user_tokens_key, token);
            actual_removed_count += 1; // Count tokens we attempt to remove from user sets
        } else {
            tracing::warn!(
                "Pubkey not found in hash for stale token: {}. Skipping user set removal.",
                token
            );
        }
    }

    // 4. Execute the pipeline
    // We expect results for ZREMRANGEBYSCORE, HDEL, and then one SREM per valid pubkey found.
    // The exact return type/value isn't critical here, just success/failure.
    let _result: RedisResult<Value> = pipe.query_async(&mut *conn).await;

    match _result {
        Ok(_) => {
            tracing::info!(
                "Successfully cleaned up {} stale tokens (attempted removal from {} user sets).",
                count,
                actual_removed_count
            );
            Ok(count) // Return the initial count of tokens identified as stale
        }
        Err(e) => {
            tracing::error!("Error during stale token cleanup pipeline: {}", e);
            Err(ServiceError::Redis(e))
        }
    }
}

/// Adds a subscription filter for a user (backward compatible - uses default app)
pub async fn add_subscription(pool: &RedisPool, pubkey: &PublicKey, filter_json: &str) -> Result<()> {
    add_subscription_with_app(pool, DEFAULT_APP, pubkey, filter_json).await
}

/// Adds a subscription filter for a user with app namespace
pub async fn add_subscription_with_app(
    pool: &RedisPool,
    app: &str,
    pubkey: &PublicKey,
    filter_json: &str,
) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    let subscriptions_key = build_subscriptions_key(app, pubkey);
    
    redis::cmd("SADD")
        .arg(&subscriptions_key)
        .arg(filter_json)
        .query_async::<()>(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    Ok(())
}

/// Removes a subscription filter for a user (backward compatible - uses default app)
pub async fn remove_subscription(pool: &RedisPool, pubkey: &PublicKey, filter_json: &str) -> Result<()> {
    remove_subscription_with_app(pool, DEFAULT_APP, pubkey, filter_json).await
}

/// Removes a subscription filter for a user with app namespace
pub async fn remove_subscription_with_app(
    pool: &RedisPool,
    app: &str,
    pubkey: &PublicKey,
    filter_json: &str,
) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    let subscriptions_key = build_subscriptions_key(app, pubkey);
    
    redis::cmd("SREM")
        .arg(&subscriptions_key)
        .arg(filter_json)
        .query_async::<()>(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    Ok(())
}

/// Gets all subscription filters for a user
pub async fn get_subscriptions(pool: &RedisPool, pubkey: &PublicKey) -> Result<Vec<String>> {
    get_subscriptions_with_app(pool, DEFAULT_APP, pubkey).await
}

/// Gets all subscription filters for a user with app namespace
pub async fn get_subscriptions_with_app(pool: &RedisPool, app: &str, pubkey: &PublicKey) -> Result<Vec<String>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    let subscriptions_key = build_subscriptions_key(app, pubkey);
    
    let subscriptions: Vec<String> = redis::cmd("SMEMBERS")
        .arg(&subscriptions_key)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    Ok(subscriptions)
}

/// Gets all users that have at least one subscription across ALL app namespaces
pub async fn get_all_users_with_subscriptions(pool: &RedisPool) -> Result<Vec<PublicKey>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    // Get all subscription keys across ALL apps
    let pattern = "app:*:subscriptions:*";
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(&pattern)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    // Also check timestamped subscriptions
    let ts_pattern = "app:*:subscriptions_ts:*";
    let ts_keys: Vec<String> = redis::cmd("KEYS")
        .arg(&ts_pattern)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    // Combine both key sets
    let all_keys = keys.into_iter().chain(ts_keys.into_iter());
    
    // Extract pubkeys from keys
    let mut pubkeys = HashSet::new();
    
    for key in all_keys {
        // Extract pubkey hex from the end of the key
        // Keys are in format "app:{app}:subscriptions:{pubkey}" or "app:{app}:subscriptions_ts:{pubkey}"
        if let Some(hex) = key.split(':').last() {
            if let Ok(pubkey) = PublicKey::from_hex(hex) {
                pubkeys.insert(pubkey);
            }
        }
    }
    
    Ok(pubkeys.into_iter().collect())
}

/// Gets all users that have at least one subscription with app namespace
pub async fn get_all_users_with_subscriptions_and_app(pool: &RedisPool, app: &str) -> Result<Vec<PublicKey>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    // Get all subscription keys for the app
    let pattern = format!("app:{}:subscriptions:*", app);
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(&pattern)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    // Also check timestamped subscriptions
    let ts_pattern = format!("app:{}:subscriptions_ts:*", app);
    let ts_keys: Vec<String> = redis::cmd("KEYS")
        .arg(&ts_pattern)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    // Combine both key sets
    let all_keys = keys.into_iter().chain(ts_keys.into_iter());
    
    // Extract pubkeys from keys
    let mut pubkeys = std::collections::HashSet::new();
    let prefix = format!("app:{}:subscriptions:", app);
    let ts_prefix = format!("app:{}:subscriptions_ts:", app);
    
    for key in all_keys {
        let hex = if let Some(h) = key.strip_prefix(&prefix) {
            h
        } else if let Some(h) = key.strip_prefix(&ts_prefix) {
            h
        } else {
            continue;
        };
        
        if let Ok(pubkey) = PublicKey::from_hex(hex) {
            pubkeys.insert(pubkey);
        }
    }
    
    Ok(pubkeys.into_iter().collect())
}

/// Add a subscription with a specific timestamp (backward compatible - uses default app)
pub async fn add_subscription_with_timestamp(
    pool: &RedisPool,
    pubkey: &PublicKey,
    filter_json: &str,
    timestamp: u64,
) -> Result<()> {
    add_subscription_with_timestamp_and_app(pool, DEFAULT_APP, pubkey, filter_json, timestamp).await
}

/// Add a subscription with a specific timestamp and app namespace
pub async fn add_subscription_with_timestamp_and_app(
    pool: &RedisPool,
    app: &str,
    pubkey: &PublicKey,
    filter_json: &str,
    timestamp: u64,
) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    // Store subscription with timestamp in a sorted set (app-namespaced)
    let subscriptions_ts_key = format!("app:{}:subscriptions_ts:{}", app, pubkey.to_hex());
    
    // Store filter with timestamp as score in sorted set
    redis::cmd("ZADD")
        .arg(&subscriptions_ts_key)
        .arg(timestamp)
        .arg(filter_json)
        .query_async::<()>(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    // Also add to regular subscriptions set for compatibility
    let regular_key = build_subscriptions_key(app, pubkey);
    redis::cmd("SADD")
        .arg(&regular_key)
        .arg(filter_json)
        .query_async::<()>(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    Ok(())
}

/// Get subscription timestamp for a filter
pub async fn get_subscription_timestamp(
    pool: &RedisPool,
    pubkey: &PublicKey,
    filter_json: &str,
) -> Result<Option<u64>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    let subscriptions_key = format!("subscriptions_ts:{}", pubkey.to_hex());
    
    let score: Option<f64> = redis::cmd("ZSCORE")
        .arg(&subscriptions_key)
        .arg(filter_json)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    Ok(score.map(|s| s as u64))
}

/// Get all subscriptions for a user with their timestamps
/// Falls back to regular subscriptions with timestamp 0 if no timestamped subscriptions exist
pub async fn get_subscriptions_with_timestamps(
    pool: &RedisPool,
    pubkey: &PublicKey,
) -> Result<Vec<(String, u64)>> {
    get_subscriptions_with_timestamps_and_app(pool, DEFAULT_APP, pubkey).await
}

/// Get all subscriptions for a user with their timestamps from ALL app namespaces
pub async fn get_all_subscriptions_with_timestamps(
    pool: &RedisPool,
    pubkey: &PublicKey,
) -> Result<Vec<(String, u64)>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    // Find all subscription keys for this pubkey across all apps
    let ts_pattern = format!("app:*:subscriptions_ts:{}", pubkey.to_hex());
    let regular_pattern = format!("app:*:subscriptions:{}", pubkey.to_hex());
    
    // Get timestamped subscriptions
    let ts_keys: Vec<String> = redis::cmd("KEYS")
        .arg(&ts_pattern)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    let mut all_subscriptions = Vec::new();
    
    // Collect timestamped subscriptions from all apps
    for key in ts_keys {
        let results: Vec<(String, f64)> = redis::cmd("ZRANGE")
            .arg(&key)
            .arg(0)
            .arg(-1)
            .arg("WITHSCORES")
            .query_async(&mut *conn)
            .await
            .map_err(ServiceError::Redis)?;
        
        for (filter, score) in results {
            all_subscriptions.push((filter, score as u64));
        }
    }
    
    // If we found timestamped subscriptions, return them
    if !all_subscriptions.is_empty() {
        return Ok(all_subscriptions);
    }
    
    // Fallback to regular subscriptions (for backward compatibility)
    let regular_keys: Vec<String> = redis::cmd("KEYS")
        .arg(&regular_pattern)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    for key in regular_keys {
        let regular_subs: Vec<String> = redis::cmd("SMEMBERS")
            .arg(&key)
            .query_async(&mut *conn)
            .await
            .map_err(ServiceError::Redis)?;
        
        // Return with timestamp 0 (will match all events)
        for filter in regular_subs {
            all_subscriptions.push((filter, 0u64));
        }
    }
    
    Ok(all_subscriptions)
}

/// Get all subscriptions for a user with their timestamps and app namespace
pub async fn get_subscriptions_with_timestamps_and_app(
    pool: &RedisPool,
    app: &str,
    pubkey: &PublicKey,
) -> Result<Vec<(String, u64)>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    let subscriptions_ts_key = format!("app:{}:subscriptions_ts:{}", app, pubkey.to_hex());
    
    // Try to get timestamped subscriptions first
    let results: Vec<(String, f64)> = redis::cmd("ZRANGE")
        .arg(&subscriptions_ts_key)
        .arg(0)
        .arg(-1)
        .arg("WITHSCORES")
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    if !results.is_empty() {
        // Convert scores to u64
        let subscriptions = results
            .into_iter()
            .map(|(filter, score)| (filter, score as u64))
            .collect();
        return Ok(subscriptions);
    }
    
    // Fallback to regular subscriptions (for backward compatibility)
    let regular_key = build_subscriptions_key(app, pubkey);
    let regular_subs: Vec<String> = redis::cmd("SMEMBERS")
        .arg(&regular_key)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    // Return with timestamp 0 (will match all events)
    let subscriptions = regular_subs
        .into_iter()
        .map(|filter| (filter, 0u64))
        .collect();
    
    Ok(subscriptions)
}

/// Get the TTL for the processed events set
pub async fn get_event_ttl(pool: &RedisPool, _event_id: &EventId) -> Result<i64> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    // The TTL is on the entire set, not individual events
    let ttl: i64 = redis::cmd("TTL")
        .arg(PROCESSED_EVENTS_SET)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    Ok(ttl)
}

/// Add a subscription filter using hash-based storage
pub async fn add_subscription_by_filter(
    pool: &RedisPool,
    app: &str,
    pubkey: &PublicKey,
    filter: &serde_json::Value,
    timestamp: u64,
) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    // Generate hash from filter
    let filter_hash = hash_filter(filter);
    
    // Store in user's subscription set using hash as the member
    let subscriptions_key = format!("app:{}:subscriptions:{}", app, pubkey.to_hex());
    redis::cmd("SADD")
        .arg(&subscriptions_key)
        .arg(&filter_hash)
        .query_async::<()>(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    // Store the filter content and timestamp by hash
    let filter_data_key = format!("app:{}:filter:{}", app, filter_hash);
    let filter_data = serde_json::json!({
        "filter": filter,
        "timestamp": timestamp,
        "pubkey": pubkey.to_hex()
    });
    
    redis::cmd("SET")
        .arg(&filter_data_key)
        .arg(serde_json::to_string(&filter_data).unwrap())
        .query_async::<()>(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    Ok(())
}

/// Remove a subscription filter using hash-based storage
pub async fn remove_subscription_by_filter(
    pool: &RedisPool,
    app: &str,
    pubkey: &PublicKey,
    filter: &serde_json::Value,
) -> Result<bool> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    // Generate hash from filter
    let filter_hash = hash_filter(filter);
    
    // Remove from user's subscription set
    let subscriptions_key = format!("app:{}:subscriptions:{}", app, pubkey.to_hex());
    let removed: i32 = redis::cmd("SREM")
        .arg(&subscriptions_key)
        .arg(&filter_hash)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    if removed > 0 {
        // Remove the filter data
        let filter_data_key = format!("app:{}:filter:{}", app, filter_hash);
        let _: () = redis::cmd("DEL")
            .arg(&filter_data_key)
            .query_async(&mut *conn)
            .await
            .map_err(ServiceError::Redis)?;
        
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Get all subscriptions for a user using hash-based storage
pub async fn get_subscriptions_by_hash(
    pool: &RedisPool,
    app: &str,
    pubkey: &PublicKey,
) -> Result<Vec<(serde_json::Value, u64)>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    // Get all filter hashes for this user
    let subscriptions_key = format!("app:{}:subscriptions:{}", app, pubkey.to_hex());
    let hashes: Vec<String> = redis::cmd("SMEMBERS")
        .arg(&subscriptions_key)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    let mut filters = Vec::new();
    
    // Retrieve each filter's data
    for hash in hashes {
        let filter_data_key = format!("app:{}:filter:{}", app, hash);
        let data_json: Option<String> = redis::cmd("GET")
            .arg(&filter_data_key)
            .query_async(&mut *conn)
            .await
            .map_err(ServiceError::Redis)?;
        
        if let Some(json_str) = data_json {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&json_str) {
                if let (Some(filter), Some(timestamp)) = (
                    data.get("filter").cloned(),
                    data.get("timestamp").and_then(|t| t.as_u64())
                ) {
                    filters.push((filter, timestamp));
                }
            }
        }
    }
    
    Ok(filters)
}

/// Get all subscriptions across all apps using hash-based storage  
pub async fn get_all_subscriptions_by_hash(
    pool: &RedisPool,
    pubkey: &PublicKey,
) -> Result<Vec<(serde_json::Value, u64)>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    // Find all subscription keys for this pubkey across all apps
    let pattern = format!("app:*:subscriptions:{}", pubkey.to_hex());
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(&pattern)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    let mut all_filters = Vec::new();
    
    for key in keys {
        // Extract app from key: app:{app}:subscriptions:{pubkey}
        let parts: Vec<&str> = key.split(':').collect();
        if parts.len() >= 4 {
            let app = parts[1];
            
            // Get filter hashes for this app
            let hashes: Vec<String> = redis::cmd("SMEMBERS")
                .arg(&key)
                .query_async(&mut *conn)
                .await
                .map_err(ServiceError::Redis)?;
            
            // Retrieve each filter's data
            for hash in hashes {
                let filter_data_key = format!("app:{}:filter:{}", app, hash);
                let data_json: Option<String> = redis::cmd("GET")
                    .arg(&filter_data_key)
                    .query_async(&mut *conn)
                    .await
                    .map_err(ServiceError::Redis)?;
                
                if let Some(json_str) = data_json {
                    if let Ok(data) = serde_json::from_str::<serde_json::Value>(&json_str) {
                        if let (Some(filter), Some(timestamp)) = (
                            data.get("filter").cloned(),
                            data.get("timestamp").and_then(|t| t.as_u64())
                        ) {
                            all_filters.push((filter, timestamp));
                        }
                    }
                }
            }
        }
    }
    
    Ok(all_filters)
}
