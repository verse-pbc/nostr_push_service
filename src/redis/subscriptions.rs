use crate::error::{Result, ServiceError};
use crate::handlers::{CommunityId, GroupId};
use nostr_sdk::prelude::*;
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, Script};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::{debug, info};

const FILTER_PREFIX: &str = "shared_filters";
const USER_FILTERS_PREFIX: &str = "user_filters";
const FILTER_USERS_PREFIX: &str = "filter_users";
const COMMUNITY_PREFIX: &str = "community_members";
const GROUP_PREFIX: &str = "group_members";
const USER_COMMUNITIES_PREFIX: &str = "user_communities";
const USER_GROUPS_PREFIX: &str = "user_groups";

#[derive(Debug, Serialize, Deserialize)]
pub struct StoredFilter {
    pub filter_json: String,
    pub ref_count: i32,
    pub created_at: i64,
    pub last_accessed: i64,
}

pub struct RedisSubscriptionStore {
    conn: ConnectionManager,
}

impl RedisSubscriptionStore {
    pub async fn new(conn: ConnectionManager) -> Self {
        RedisSubscriptionStore { conn }
    }
    
    // Shared filter operations
    pub async fn store_shared_filter(&mut self, filter_hash: &str, filter: &Filter) -> Result<()> {
        let key = format!("{}:{}", FILTER_PREFIX, filter_hash);
        
        let stored_filter = StoredFilter {
            filter_json: serde_json::to_string(filter)?,
            ref_count: 1,
            created_at: chrono::Utc::now().timestamp(),
            last_accessed: chrono::Utc::now().timestamp(),
        };
        
        let value = serde_json::to_string(&stored_filter)?;
        self.conn.set::<_, _, ()>(&key, value).await?;
        
        debug!("Stored shared filter with hash: {}", &filter_hash[..8]);
        Ok(())
    }
    
    pub async fn get_shared_filter(&mut self, filter_hash: &str) -> Result<Option<Filter>> {
        let key = format!("{}:{}", FILTER_PREFIX, filter_hash);
        
        let value: Option<String> = self.conn.get(&key).await?;
        
        if let Some(json) = value {
            let stored: StoredFilter = serde_json::from_str(&json)?;
            let filter = serde_json::from_str(&stored.filter_json)?;
            
            // Update last accessed time
            let updated = StoredFilter {
                last_accessed: chrono::Utc::now().timestamp(),
                ..stored
            };
            let updated_json = serde_json::to_string(&updated)?;
            self.conn.set::<_, _, ()>(&key, updated_json).await?;
            
            Ok(Some(filter))
        } else {
            Ok(None)
        }
    }
    
    pub async fn increment_filter_ref_count(&mut self, filter_hash: &str) -> Result<i32> {
        let key = format!("{}:{}", FILTER_PREFIX, filter_hash);
        
        // Use Lua script for atomic read-modify-write
        let script = Script::new(r#"
            local key = KEYS[1]
            local value = redis.call('GET', key)
            if value then
                local data = cjson.decode(value)
                data.ref_count = data.ref_count + 1
                data.last_accessed = tonumber(ARGV[1])
                redis.call('SET', key, cjson.encode(data))
                return data.ref_count
            else
                return nil
            end
        "#);
        
        let now = chrono::Utc::now().timestamp();
        let result: Option<i32> = script
            .key(&key)
            .arg(now)
            .invoke_async(&mut self.conn)
            .await?;
        
        result.ok_or_else(|| ServiceError::Internal(format!("Filter {} not found", filter_hash)))
    }
    
    pub async fn decrement_filter_ref_count(&mut self, filter_hash: &str) -> Result<i32> {
        let key = format!("{}:{}", FILTER_PREFIX, filter_hash);
        
        // Use Lua script for atomic read-modify-write with cleanup
        let script = Script::new(r#"
            local key = KEYS[1]
            local value = redis.call('GET', key)
            if value then
                local data = cjson.decode(value)
                data.ref_count = data.ref_count - 1
                if data.ref_count <= 0 then
                    redis.call('DEL', key)
                    return 0
                else
                    data.last_accessed = tonumber(ARGV[1])
                    redis.call('SET', key, cjson.encode(data))
                    return data.ref_count
                end
            else
                return nil
            end
        "#);
        
        let now = chrono::Utc::now().timestamp();
        let result: Option<i32> = script
            .key(&key)
            .arg(now)
            .invoke_async(&mut self.conn)
            .await?;
        
        Ok(result.unwrap_or(0))
    }
    
    // User filter tracking
    pub async fn add_user_filter(&mut self, app: &str, pubkey: &str, filter_hash: &str) -> Result<()> {
        let user_key = format!("{}:{}:{}", USER_FILTERS_PREFIX, app, pubkey);
        let filter_key = format!("{}:{}", FILTER_USERS_PREFIX, filter_hash);
        
        // Add filter to user's set
        self.conn.sadd::<_, _, ()>(&user_key, filter_hash).await?;
        
        // Add user to filter's set
        let user_id = format!("{}:{}", app, pubkey);
        self.conn.sadd::<_, _, ()>(&filter_key, &user_id).await?;
        
        debug!("Added filter {} for user {}:{}", &filter_hash[..8], app, pubkey);
        Ok(())
    }
    
    pub async fn remove_user_filter(&mut self, app: &str, pubkey: &str, filter_hash: &str) -> Result<()> {
        let user_key = format!("{}:{}:{}", USER_FILTERS_PREFIX, app, pubkey);
        let filter_key = format!("{}:{}", FILTER_USERS_PREFIX, filter_hash);
        
        // Remove filter from user's set
        self.conn.srem::<_, _, ()>(&user_key, filter_hash).await?;
        
        // Remove user from filter's set
        let user_id = format!("{}:{}", app, pubkey);
        self.conn.srem::<_, _, ()>(&filter_key, &user_id).await?;
        
        // Clean up empty sets
        let user_count: usize = self.conn.scard(&user_key).await?;
        if user_count == 0 {
            self.conn.del::<_, ()>(&user_key).await?;
        }
        
        let filter_count: usize = self.conn.scard(&filter_key).await?;
        if filter_count == 0 {
            self.conn.del::<_, ()>(&filter_key).await?;
        }
        
        debug!("Removed filter {} for user {}:{}", &filter_hash[..8], app, pubkey);
        Ok(())
    }
    
    pub async fn get_user_filters(&mut self, app: &str, pubkey: &str) -> Result<HashSet<String>> {
        let key = format!("{}:{}:{}", USER_FILTERS_PREFIX, app, pubkey);
        let filters: HashSet<String> = self.conn.smembers(&key).await?;
        Ok(filters)
    }
    
    pub async fn get_filter_users(&mut self, filter_hash: &str) -> Result<HashSet<String>> {
        let key = format!("{}:{}", FILTER_USERS_PREFIX, filter_hash);
        let users: HashSet<String> = self.conn.smembers(&key).await?;
        Ok(users)
    }
    
    // Community operations
    pub async fn add_community_member(&mut self, community: &CommunityId, pubkey: &str) -> Result<()> {
        let community_key = format!("{}:{}", COMMUNITY_PREFIX, community.to_redis_key());
        let user_key = format!("{}:{}", USER_COMMUNITIES_PREFIX, pubkey);
        
        self.conn.sadd::<_, _, ()>(&community_key, pubkey).await?;
        self.conn.sadd::<_, _, ()>(&user_key, community.to_a_tag()).await?;
        
        debug!("Added {} to community {}", pubkey, community.to_a_tag());
        Ok(())
    }
    
    pub async fn remove_community_member(&mut self, community: &CommunityId, pubkey: &str) -> Result<()> {
        let community_key = format!("{}:{}", COMMUNITY_PREFIX, community.to_redis_key());
        let user_key = format!("{}:{}", USER_COMMUNITIES_PREFIX, pubkey);
        
        self.conn.srem::<_, _, ()>(&community_key, pubkey).await?;
        self.conn.srem::<_, _, ()>(&user_key, community.to_a_tag()).await?;
        
        // Clean up empty sets
        let member_count: usize = self.conn.scard(&community_key).await?;
        if member_count == 0 {
            self.conn.del::<_, ()>(&community_key).await?;
        }
        
        let community_count: usize = self.conn.scard(&user_key).await?;
        if community_count == 0 {
            self.conn.del::<_, ()>(&user_key).await?;
        }
        
        debug!("Removed {} from community {}", pubkey, community.to_a_tag());
        Ok(())
    }
    
    pub async fn get_community_members(&mut self, community: &CommunityId) -> Result<HashSet<String>> {
        let key = format!("{}:{}", COMMUNITY_PREFIX, community.to_redis_key());
        let members: HashSet<String> = self.conn.smembers(&key).await?;
        Ok(members)
    }
    
    // Group operations
    pub async fn add_group_member(&mut self, group: &GroupId, pubkey: &str) -> Result<()> {
        let group_key = format!("{}:{}", GROUP_PREFIX, group.to_redis_key());
        let user_key = format!("{}:{}", USER_GROUPS_PREFIX, pubkey);
        
        self.conn.sadd::<_, _, ()>(&group_key, pubkey).await?;
        self.conn.sadd::<_, _, ()>(&user_key, format!("{}:{}", group.relay_url, group.group_name)).await?;
        
        debug!("Added {} to group {}", pubkey, group.to_h_tag());
        Ok(())
    }
    
    pub async fn get_group_members(&mut self, group: &GroupId) -> Result<HashSet<String>> {
        let key = format!("{}:{}", GROUP_PREFIX, group.to_redis_key());
        let members: HashSet<String> = self.conn.smembers(&key).await?;
        Ok(members)
    }
    
    // Cleanup operations
    pub async fn cleanup_user(&mut self, app: &str, pubkey: &str) -> Result<()> {
        // Get all user's filters
        let filters = self.get_user_filters(app, pubkey).await?;
        
        // Remove each filter
        for filter_hash in filters {
            self.remove_user_filter(app, pubkey, &filter_hash).await?;
            self.decrement_filter_ref_count(&filter_hash).await?;
        }
        
        // Clean up community memberships
        let communities_key = format!("{}:{}", USER_COMMUNITIES_PREFIX, pubkey);
        let communities: HashSet<String> = self.conn.smembers(&communities_key).await?;
        
        for community_tag in communities {
            if let Some(community) = CommunityId::from_a_tag(&community_tag) {
                self.remove_community_member(&community, pubkey).await?;
            }
        }
        
        // Clean up group memberships
        let groups_key = format!("{}:{}", USER_GROUPS_PREFIX, pubkey);
        self.conn.del::<_, ()>(&groups_key).await?;
        
        info!("Cleaned up all Redis data for user {}:{}", app, pubkey);
        Ok(())
    }
    
    // Stats and monitoring
    pub async fn get_stats(&mut self) -> Result<HashMap<String, usize>> {
        let mut stats = HashMap::new();
        
        // Count shared filters
        let filter_keys: Vec<String> = self.conn.keys(format!("{}:*", FILTER_PREFIX)).await?;
        stats.insert("shared_filters".to_string(), filter_keys.len());
        
        // Count users with filters
        let user_keys: Vec<String> = self.conn.keys(format!("{}:*", USER_FILTERS_PREFIX)).await?;
        stats.insert("users_with_filters".to_string(), user_keys.len());
        
        // Count communities
        let community_keys: Vec<String> = self.conn.keys(format!("{}:*", COMMUNITY_PREFIX)).await?;
        stats.insert("communities".to_string(), community_keys.len());
        
        // Count groups
        let group_keys: Vec<String> = self.conn.keys(format!("{}:*", GROUP_PREFIX)).await?;
        stats.insert("groups".to_string(), group_keys.len());
        
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::Client;
    
    async fn get_test_connection() -> Option<ConnectionManager> {
        use tokio::time::{timeout, Duration};

        if let Ok(client) = Client::open("redis://127.0.0.1:6379") {
            // Timeout after 2 seconds to fail fast when Redis unavailable
            match timeout(Duration::from_secs(2), ConnectionManager::new(client)).await {
                Ok(Ok(conn)) => return Some(conn),
                Ok(Err(_)) => {
                    eprintln!("Redis connection failed - start Redis with: redis-server");
                    return None;
                }
                Err(_) => {
                    eprintln!("Redis connection timeout - is Redis running? Start with: redis-server");
                    return None;
                }
            }
        }
        None
    }
    
    #[tokio::test]
    async fn test_filter_ref_counting() {
        let conn = match get_test_connection().await {
            Some(c) => c,
            None => {
                println!("Skipping test - Redis not available");
                return;
            }
        };
        
        let mut store = RedisSubscriptionStore::new(conn).await;
        
        let filter = Filter::new().kinds([Kind::from(1)]);
        let filter_hash = "test_filter_hash";
        
        // Store filter
        store.store_shared_filter(filter_hash, &filter).await.unwrap();
        
        // Increment ref count
        let count = store.increment_filter_ref_count(filter_hash).await.unwrap();
        assert_eq!(count, 2);
        
        // Decrement ref count
        let count = store.decrement_filter_ref_count(filter_hash).await.unwrap();
        assert_eq!(count, 1);
        
        // Decrement to zero (should delete)
        let count = store.decrement_filter_ref_count(filter_hash).await.unwrap();
        assert_eq!(count, 0);
        
        // Filter should be gone
        let result = store.get_shared_filter(filter_hash).await.unwrap();
        assert!(result.is_none());
    }
}