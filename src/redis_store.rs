use crate::error::{Result, ServiceError};
use bb8_redis::bb8::Pool;
use bb8_redis::RedisConnectionManager;
use nostr_sdk::{EventId, PublicKey, Timestamp};
use redis::{RedisResult, Value};
use std::convert::TryInto;
use std::time::Duration;

// Type alias for the connection pool
pub type RedisPool = Pool<RedisConnectionManager>;

// Constants for Redis keys and sets
const PROCESSED_EVENTS_SET: &str = "processed_nostr_events";
const PUBKEY_DEVICE_TOKENS_SET_PREFIX: &str = "user_tokens:";
const STALE_TOKENS_ZSET: &str = "stale_tokens";
pub const TOKEN_TO_PUBKEY_HASH: &str = "token_to_pubkey";
const SUBSCRIPTIONS_SET_PREFIX: &str = "subscriptions:";

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

/// Retrieves device tokens associated with a public key.
pub async fn get_tokens_for_pubkey(pool: &RedisPool, pubkey: &PublicKey) -> Result<Vec<String>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    let user_tokens_key = format!("{}{}", PUBKEY_DEVICE_TOKENS_SET_PREFIX, pubkey.to_hex());

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

/// Adds or updates a single device token for a pubkey, updating its last seen time in the ZSET.
pub async fn add_or_update_token(pool: &RedisPool, pubkey: &PublicKey, token: &str) -> Result<()> {
    let now_timestamp = Timestamp::now().as_u64();
    let pubkey_hex = pubkey.to_hex();
    let user_tokens_key = format!("{}{}", PUBKEY_DEVICE_TOKENS_SET_PREFIX, pubkey_hex);

    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;

    // Use a pipeline to ensure atomicity
    let mut pipe = redis::pipe();
    pipe.atomic()
        .sadd(&user_tokens_key, token) // Add token to user's set
        .zadd(STALE_TOKENS_ZSET, token, now_timestamp) // Add/update token in sorted set with current timestamp
        .hset(TOKEN_TO_PUBKEY_HASH, token, &pubkey_hex); // Map token back to pubkey

    pipe.query_async::<Value>(&mut *conn) // Specify the expected return type 'Value'
        .await
        .map(|_| ()) // Discard the pipeline result, interested only in errors
        .map_err(ServiceError::Redis)
}

/// Removes a single device token for a pubkey.
/// Returns true if the token was found and removed.
pub async fn remove_token(pool: &RedisPool, pubkey: &PublicKey, token: &str) -> Result<bool> {
    let removed = remove_device_tokens(pool, pubkey, &[token.to_string()]).await?;
    Ok(removed > 0)
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

/// Adds a subscription filter for a user
pub async fn add_subscription(pool: &RedisPool, pubkey: &PublicKey, filter_json: &str) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    let subscriptions_key = format!("{}{}", SUBSCRIPTIONS_SET_PREFIX, pubkey.to_hex());
    
    redis::cmd("SADD")
        .arg(&subscriptions_key)
        .arg(filter_json)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    Ok(())
}

/// Removes a subscription filter for a user
pub async fn remove_subscription(pool: &RedisPool, pubkey: &PublicKey, filter_json: &str) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    let subscriptions_key = format!("{}{}", SUBSCRIPTIONS_SET_PREFIX, pubkey.to_hex());
    
    redis::cmd("SREM")
        .arg(&subscriptions_key)
        .arg(filter_json)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    Ok(())
}

/// Gets all subscription filters for a user
pub async fn get_subscriptions(pool: &RedisPool, pubkey: &PublicKey) -> Result<Vec<String>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    let subscriptions_key = format!("{}{}", SUBSCRIPTIONS_SET_PREFIX, pubkey.to_hex());
    
    let subscriptions: Vec<String> = redis::cmd("SMEMBERS")
        .arg(&subscriptions_key)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    Ok(subscriptions)
}

/// Gets all users that have at least one subscription
pub async fn get_all_users_with_subscriptions(pool: &RedisPool) -> Result<Vec<PublicKey>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| ServiceError::Internal(format!("Failed to get Redis connection: {}", e)))?;
    
    // Get all subscription keys
    let pattern = format!("{}*", SUBSCRIPTIONS_SET_PREFIX);
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(&pattern)
        .query_async(&mut *conn)
        .await
        .map_err(ServiceError::Redis)?;
    
    // Extract pubkeys from keys
    let mut pubkeys = Vec::new();
    for key in keys {
        if let Some(hex) = key.strip_prefix(SUBSCRIPTIONS_SET_PREFIX) {
            if let Ok(pubkey) = PublicKey::from_hex(hex) {
                pubkeys.push(pubkey);
            }
        }
    }
    
    Ok(pubkeys)
}
