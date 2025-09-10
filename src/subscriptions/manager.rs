use blake3;
use nostr_sdk::prelude::*;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

// Type aliases to simplify complex types
type UserKey = (String, String); // (app, pubkey)
type FilterHashes = HashSet<String>;
type UserSet = HashSet<(String, String)>;

#[derive(Debug, Clone)]
pub struct SharedFilter {
    pub filter: Filter,
    pub filter_hash: String,
    pub ref_count: i32,
}

#[derive(Debug)]
pub struct SubscriptionManager {
    shared_filters: Arc<RwLock<HashMap<String, SharedFilter>>>,
    user_filters: Arc<RwLock<HashMap<UserKey, FilterHashes>>>, // (app, pubkey) -> filter_hashes
    filter_users: Arc<RwLock<HashMap<String, UserSet>>>, // filter_hash -> (app, pubkey) set
}

impl Default for SubscriptionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SubscriptionManager {
    pub fn new() -> Self {
        Self {
            shared_filters: Arc::new(RwLock::new(HashMap::new())),
            user_filters: Arc::new(RwLock::new(HashMap::new())),
            filter_users: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn normalize_filter(filter: &Filter) -> Value {
        let filter_json = serde_json::to_value(filter).unwrap();
        let mut normalized = filter_json.as_object().unwrap().clone();
        
        // Remove temporal parameters that don't affect subscription sharing
        normalized.remove("limit");
        normalized.remove("since");
        normalized.remove("until");
        
        // Sort and deduplicate kinds
        if let Some(kinds) = normalized.get_mut("kinds") {
            if let Some(kinds_array) = kinds.as_array_mut() {
                kinds_array.sort_by(|a, b| {
                    a.as_u64().unwrap().cmp(&b.as_u64().unwrap())
                });
                kinds_array.dedup();
            }
        }
        
        // Sort and deduplicate authors
        if let Some(authors) = normalized.get_mut("authors") {
            if let Some(authors_array) = authors.as_array_mut() {
                authors_array.sort_by(|a, b| {
                    a.as_str().unwrap().cmp(b.as_str().unwrap())
                });
                authors_array.dedup();
            }
        }
        
        // Sort and deduplicate tag values
        for tag_key in ["#e", "#p", "#a", "#t", "#d", "#r", "#h", "#g"] {
            if let Some(tags) = normalized.get_mut(tag_key) {
                if let Some(tag_array) = tags.as_array_mut() {
                    tag_array.sort_by(|a, b| {
                        a.as_str().unwrap_or("").cmp(b.as_str().unwrap_or(""))
                    });
                    tag_array.dedup();
                }
            }
        }
        
        json!(normalized)
    }

    pub fn compute_filter_hash(filter: &Filter) -> String {
        let normalized_json = Self::normalize_filter(filter);
        let serialized = serde_json::to_string(&normalized_json).unwrap();
        
        let hash = blake3::hash(serialized.as_bytes());
        hash.to_hex().to_string()
    }

    pub async fn add_user_filter(&self, app: String, pubkey: String, filter: Filter) -> (String, bool) {
        let filter_hash = Self::compute_filter_hash(&filter);
        
        // Update or create shared filter and track if it's new
        let is_new_filter = {
            let mut shared_filters = self.shared_filters.write().await;
            let shared_filter = shared_filters.entry(filter_hash.clone()).or_insert_with(|| {
                info!("Creating new shared filter with hash: {}", &filter_hash[..8]);
                SharedFilter {
                    filter: filter.clone(),
                    filter_hash: filter_hash.clone(),
                    ref_count: 0,
                }
            });
            let old_ref_count = shared_filter.ref_count;
            shared_filter.ref_count += 1;
            debug!("Filter {} ref_count incremented to {}", &filter_hash[..8], shared_filter.ref_count);
            old_ref_count == 0  // It's new if ref_count was 0 before incrementing
        };
        
        // Track user's filters
        {
            let mut user_filters = self.user_filters.write().await;
            let user_key = (app.clone(), pubkey.clone());
            user_filters.entry(user_key.clone())
                .or_insert_with(HashSet::new)
                .insert(filter_hash.clone());
        }
        
        // Track filter's users
        {
            let mut filter_users = self.filter_users.write().await;
            let user_key = (app, pubkey);
            filter_users.entry(filter_hash.clone())
                .or_insert_with(HashSet::new)
                .insert(user_key);
        }
        
        (filter_hash, is_new_filter)
    }

    pub async fn remove_user_filter(&self, app: &str, pubkey: &str, filter_hash: &str) -> bool {
        let user_key = (app.to_string(), pubkey.to_string());
        
        // Remove from user's filter set
        {
            let mut user_filters = self.user_filters.write().await;
            if let Some(user_filter_set) = user_filters.get_mut(&user_key) {
                user_filter_set.remove(filter_hash);
                if user_filter_set.is_empty() {
                    user_filters.remove(&user_key);
                }
            }
        }
        
        // Remove user from filter's user set
        {
            let mut filter_users = self.filter_users.write().await;
            if let Some(filter_user_set) = filter_users.get_mut(filter_hash) {
                filter_user_set.remove(&user_key);
                if filter_user_set.is_empty() {
                    filter_users.remove(filter_hash);
                }
            }
        }
        
        // Decrement ref count and remove if zero
        {
            let mut shared_filters = self.shared_filters.write().await;
            if let Some(shared_filter) = shared_filters.get_mut(filter_hash) {
                shared_filter.ref_count -= 1;
                debug!("Filter {} ref_count decremented to {}", &filter_hash[..8], shared_filter.ref_count);
                
                if shared_filter.ref_count <= 0 {
                    shared_filters.remove(filter_hash);
                    info!("Removed shared filter {} (ref_count reached 0)", &filter_hash[..8]);
                    return true; // Filter was removed
                }
            }
        }
        
        false // Filter still has references
    }

    pub async fn remove_all_user_filters(&self, app: &str, pubkey: &str) -> Vec<String> {
        let user_key = (app.to_string(), pubkey.to_string());
        let mut removed_filters = Vec::new();
        
        // Get all user's filters
        let filter_hashes = {
            let user_filters = self.user_filters.read().await;
            user_filters.get(&user_key).cloned().unwrap_or_default()
        };
        
        // Remove each filter
        for filter_hash in filter_hashes {
            if self.remove_user_filter(app, pubkey, &filter_hash).await {
                removed_filters.push(filter_hash);
            }
        }
        
        info!("Removed {} filters for user {}:{}", removed_filters.len(), app, pubkey);
        removed_filters
    }

    pub async fn get_ref_count(&self, filter_hash: &str) -> i32 {
        let shared_filters = self.shared_filters.read().await;
        shared_filters.get(filter_hash).map(|sf| sf.ref_count).unwrap_or(0)
    }

    pub async fn get_user_filters(&self, app: &str, pubkey: &str) -> HashSet<String> {
        let user_filters = self.user_filters.read().await;
        user_filters.get(&(app.to_string(), pubkey.to_string()))
            .cloned()
            .unwrap_or_default()
    }

    pub async fn get_filter_users(&self, filter_hash: &str) -> HashSet<(String, String)> {
        let filter_users = self.filter_users.read().await;
        filter_users.get(filter_hash)
            .cloned()
            .unwrap_or_default()
    }

    pub async fn get_all_shared_filters(&self) -> Vec<Filter> {
        let shared_filters = self.shared_filters.read().await;
        shared_filters.values()
            .map(|sf| sf.filter.clone())
            .collect()
    }

    pub async fn get_unique_filters_count(&self) -> usize {
        let shared_filters = self.shared_filters.read().await;
        shared_filters.len()
    }

    pub async fn get_total_subscriptions_count(&self) -> usize {
        let user_filters = self.user_filters.read().await;
        user_filters.values()
            .map(|filters| filters.len())
            .sum()
    }

    pub async fn cleanup_user_on_disconnect(&self, app: &str, pubkey: &str) {
        let removed = self.remove_all_user_filters(app, pubkey).await;
        if !removed.is_empty() {
            info!("Cleaned up {} subscriptions for disconnected user {}:{}", 
                removed.len(), app, pubkey);
        }
    }

    pub async fn get_filter_by_hash(&self, filter_hash: &str) -> Option<Filter> {
        let shared_filters = self.shared_filters.read().await;
        shared_filters.get(filter_hash).map(|sf| sf.filter.clone())
    }

    pub async fn has_active_subscriptions(&self, app: &str, pubkey: &str) -> bool {
        let user_filters = self.user_filters.read().await;
        user_filters.get(&(app.to_string(), pubkey.to_string()))
            .map(|filters| !filters.is_empty())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_add_and_remove_filter() {
        let manager = SubscriptionManager::new();
        
        let filter = Filter::new().kinds([Kind::from(1)]);
        let (hash, _is_new) = manager.add_user_filter("app1".to_string(), "user1".to_string(), filter).await;
        
        assert_eq!(manager.get_ref_count(&hash).await, 1);
        
        let removed = manager.remove_user_filter("app1", "user1", &hash).await;
        assert!(removed);
        assert_eq!(manager.get_ref_count(&hash).await, 0);
    }

    #[tokio::test]
    async fn test_filter_sharing() {
        let manager = SubscriptionManager::new();
        
        let filter = Filter::new().kinds([Kind::from(1)]).limit(100);
        
        let (hash1, _is_new1) = manager.add_user_filter("app1".to_string(), "user1".to_string(), filter.clone()).await;
        let (hash2, _is_new2) = manager.add_user_filter("app1".to_string(), "user2".to_string(), filter).await;
        
        assert_eq!(hash1, hash2);
        assert_eq!(manager.get_ref_count(&hash1).await, 2);
        assert_eq!(manager.get_unique_filters_count().await, 1);
    }

    #[tokio::test]
    async fn test_cleanup_on_disconnect() {
        let manager = SubscriptionManager::new();
        
        let filter1 = Filter::new().kinds([Kind::from(1)]);
        let filter2 = Filter::new().kinds([Kind::from(3)]);
        
        let (_hash1, _) = manager.add_user_filter("app1".to_string(), "user1".to_string(), filter1).await;
        let (_hash2, _) = manager.add_user_filter("app1".to_string(), "user1".to_string(), filter2).await;
        
        assert_eq!(manager.get_user_filters("app1", "user1").await.len(), 2);
        
        manager.cleanup_user_on_disconnect("app1", "user1").await;
        
        assert_eq!(manager.get_user_filters("app1", "user1").await.len(), 0);
        assert_eq!(manager.get_unique_filters_count().await, 0);
    }
}