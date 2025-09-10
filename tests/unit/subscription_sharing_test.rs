use blake3;
use nostr_sdk::prelude::*;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
struct SharedFilter {
    filter: Filter,
    filter_hash: String,
    ref_count: i32,
}

#[derive(Debug)]
struct SubscriptionManager {
    shared_filters: HashMap<String, SharedFilter>,
    user_filters: HashMap<(String, String), HashSet<String>>, // (app, pubkey) -> filter_hashes
    filter_users: HashMap<String, HashSet<(String, String)>>, // filter_hash -> (app, pubkey) set
}

impl SubscriptionManager {
    fn new() -> Self {
        Self {
            shared_filters: HashMap::new(),
            user_filters: HashMap::new(),
            filter_users: HashMap::new(),
        }
    }

    fn normalize_filter(filter: &Filter) -> Value {
        let filter_json = serde_json::to_value(filter).unwrap();
        let mut normalized = filter_json.as_object().unwrap().clone();
        
        normalized.remove("limit");
        normalized.remove("since");
        normalized.remove("until");
        
        if let Some(kinds) = normalized.get_mut("kinds") {
            if let Some(kinds_array) = kinds.as_array_mut() {
                kinds_array.sort_by(|a, b| {
                    a.as_u64().unwrap().cmp(&b.as_u64().unwrap())
                });
                kinds_array.dedup();
            }
        }
        
        if let Some(authors) = normalized.get_mut("authors") {
            if let Some(authors_array) = authors.as_array_mut() {
                authors_array.sort_by(|a, b| {
                    a.as_str().unwrap().cmp(&b.as_str().unwrap())
                });
                authors_array.dedup();
            }
        }
        
        json!(normalized)
    }

    fn compute_filter_hash(filter: &Filter) -> String {
        let normalized_json = Self::normalize_filter(filter);
        let serialized = serde_json::to_string(&normalized_json).unwrap();
        
        let hash = blake3::hash(serialized.as_bytes());
        hash.to_hex().to_string()
    }

    fn add_user_filter(&mut self, app: String, pubkey: String, filter: Filter) -> String {
        let filter_hash = Self::compute_filter_hash(&filter);
        
        // Update or create shared filter
        let shared_filter = self.shared_filters.entry(filter_hash.clone()).or_insert_with(|| {
            SharedFilter {
                filter: filter.clone(),
                filter_hash: filter_hash.clone(),
                ref_count: 0,
            }
        });
        shared_filter.ref_count += 1;
        
        // Track user's filters
        let user_key = (app.clone(), pubkey.clone());
        self.user_filters.entry(user_key.clone()).or_insert_with(HashSet::new).insert(filter_hash.clone());
        
        // Track filter's users
        self.filter_users.entry(filter_hash.clone()).or_insert_with(HashSet::new).insert(user_key);
        
        filter_hash
    }

    fn remove_user_filter(&mut self, app: &str, pubkey: &str, filter_hash: &str) -> bool {
        let user_key = (app.to_string(), pubkey.to_string());
        
        // Remove from user's filter set
        if let Some(user_filter_set) = self.user_filters.get_mut(&user_key) {
            user_filter_set.remove(filter_hash);
            if user_filter_set.is_empty() {
                self.user_filters.remove(&user_key);
            }
        }
        
        // Remove user from filter's user set
        if let Some(filter_user_set) = self.filter_users.get_mut(filter_hash) {
            filter_user_set.remove(&user_key);
            if filter_user_set.is_empty() {
                self.filter_users.remove(filter_hash);
            }
        }
        
        // Decrement ref count and remove if zero
        if let Some(shared_filter) = self.shared_filters.get_mut(filter_hash) {
            shared_filter.ref_count -= 1;
            if shared_filter.ref_count <= 0 {
                self.shared_filters.remove(filter_hash);
                return true; // Filter was removed
            }
        }
        
        false // Filter still has references
    }

    fn get_ref_count(&self, filter_hash: &str) -> i32 {
        self.shared_filters.get(filter_hash).map(|sf| sf.ref_count).unwrap_or(0)
    }

    fn get_user_filters(&self, app: &str, pubkey: &str) -> HashSet<String> {
        self.user_filters.get(&(app.to_string(), pubkey.to_string()))
            .cloned()
            .unwrap_or_default()
    }

    fn get_filter_users(&self, filter_hash: &str) -> HashSet<(String, String)> {
        self.filter_users.get(filter_hash)
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multiple_users_share_same_filter() {
        let mut manager = SubscriptionManager::new();
        
        let filter = Filter::new()
            .kinds([Kind::from(1), Kind::from(3)])
            .limit(100); // This will be normalized away
        
        // User 1 adds the filter
        let hash1 = manager.add_user_filter(
            "app1".to_string(),
            "user1".to_string(),
            filter.clone()
        );
        
        // User 2 adds the same filter
        let hash2 = manager.add_user_filter(
            "app1".to_string(),
            "user2".to_string(),
            filter.clone()
        );
        
        // Both should get the same hash
        assert_eq!(hash1, hash2, "Same filter should produce same hash");
        
        // Reference count should be 2
        assert_eq!(manager.get_ref_count(&hash1), 2, "Ref count should be 2");
        
        // Only one shared filter should exist
        assert_eq!(manager.shared_filters.len(), 1, "Only one shared filter should exist");
    }

    #[test]
    fn test_reference_counting() {
        let mut manager = SubscriptionManager::new();
        
        let filter = Filter::new().kinds([Kind::from(1)]);
        
        // Add filter for 3 users
        let hash = manager.add_user_filter("app1".to_string(), "user1".to_string(), filter.clone());
        manager.add_user_filter("app1".to_string(), "user2".to_string(), filter.clone());
        manager.add_user_filter("app1".to_string(), "user3".to_string(), filter.clone());
        
        assert_eq!(manager.get_ref_count(&hash), 3, "Ref count should be 3");
        
        // Remove one user
        manager.remove_user_filter("app1", "user1", &hash);
        assert_eq!(manager.get_ref_count(&hash), 2, "Ref count should be 2");
        
        // Remove another user
        manager.remove_user_filter("app1", "user2", &hash);
        assert_eq!(manager.get_ref_count(&hash), 1, "Ref count should be 1");
        
        // Filter should still exist
        assert!(manager.shared_filters.contains_key(&hash), "Filter should still exist");
    }

    #[test]
    fn test_filter_cleanup_when_ref_count_zero() {
        let mut manager = SubscriptionManager::new();
        
        let filter = Filter::new().kinds([Kind::from(1)]);
        
        // Add filter for 2 users
        let hash = manager.add_user_filter("app1".to_string(), "user1".to_string(), filter.clone());
        manager.add_user_filter("app1".to_string(), "user2".to_string(), filter.clone());
        
        assert_eq!(manager.shared_filters.len(), 1, "One filter should exist");
        
        // Remove both users
        let removed1 = manager.remove_user_filter("app1", "user1", &hash);
        assert!(!removed1, "Filter should not be removed yet");
        
        let removed2 = manager.remove_user_filter("app1", "user2", &hash);
        assert!(removed2, "Filter should be removed when ref count reaches zero");
        
        // Filter should be cleaned up
        assert_eq!(manager.shared_filters.len(), 0, "No filters should exist");
        assert_eq!(manager.get_ref_count(&hash), 0, "Ref count should be 0");
    }

    #[test]
    fn test_user_filter_tracking() {
        let mut manager = SubscriptionManager::new();
        
        let filter1 = Filter::new().kinds([Kind::from(1)]);
        let filter2 = Filter::new().kinds([Kind::from(3)]);
        let filter3 = Filter::new().kinds([Kind::from(7)]);
        
        // User 1 has filter1 and filter2
        let hash1 = manager.add_user_filter("app1".to_string(), "user1".to_string(), filter1.clone());
        let hash2 = manager.add_user_filter("app1".to_string(), "user1".to_string(), filter2.clone());
        
        // User 2 has filter2 and filter3
        manager.add_user_filter("app1".to_string(), "user2".to_string(), filter2.clone());
        let hash3 = manager.add_user_filter("app1".to_string(), "user2".to_string(), filter3.clone());
        
        // Check user1's filters
        let user1_filters = manager.get_user_filters("app1", "user1");
        assert_eq!(user1_filters.len(), 2, "User1 should have 2 filters");
        assert!(user1_filters.contains(&hash1), "User1 should have filter1");
        assert!(user1_filters.contains(&hash2), "User1 should have filter2");
        
        // Check user2's filters
        let user2_filters = manager.get_user_filters("app1", "user2");
        assert_eq!(user2_filters.len(), 2, "User2 should have 2 filters");
        assert!(user2_filters.contains(&hash2), "User2 should have filter2");
        assert!(user2_filters.contains(&hash3), "User2 should have filter3");
        
        // Check filter2's users (shared by both)
        let filter2_users = manager.get_filter_users(&hash2);
        assert_eq!(filter2_users.len(), 2, "Filter2 should have 2 users");
        assert!(filter2_users.contains(&("app1".to_string(), "user1".to_string())));
        assert!(filter2_users.contains(&("app1".to_string(), "user2".to_string())));
    }

    #[test]
    fn test_different_apps_same_user() {
        let mut manager = SubscriptionManager::new();
        
        let filter = Filter::new().kinds([Kind::from(1)]);
        
        // Same user, different apps
        let hash1 = manager.add_user_filter("app1".to_string(), "user1".to_string(), filter.clone());
        let hash2 = manager.add_user_filter("app2".to_string(), "user1".to_string(), filter.clone());
        
        // Should share the same filter
        assert_eq!(hash1, hash2, "Same filter should produce same hash");
        assert_eq!(manager.get_ref_count(&hash1), 2, "Ref count should be 2");
        
        // But tracked separately per app
        let app1_filters = manager.get_user_filters("app1", "user1");
        let app2_filters = manager.get_user_filters("app2", "user1");
        
        assert_eq!(app1_filters.len(), 1);
        assert_eq!(app2_filters.len(), 1);
        assert_eq!(app1_filters, app2_filters, "Both apps should have same filter hash");
    }

    #[test]
    fn test_normalized_filters_share_subscription() {
        let mut manager = SubscriptionManager::new();
        
        // Filters that differ only in limit/since/until
        let filter1 = Filter::new()
            .kinds([Kind::from(1)])
            .limit(100);
            
        let filter2 = Filter::new()
            .kinds([Kind::from(1)])
            .limit(200)
            .since(Timestamp::now());
        
        let hash1 = manager.add_user_filter("app1".to_string(), "user1".to_string(), filter1);
        let hash2 = manager.add_user_filter("app1".to_string(), "user2".to_string(), filter2);
        
        // Should share the same subscription
        assert_eq!(hash1, hash2, "Normalized filters should share subscription");
        assert_eq!(manager.shared_filters.len(), 1, "Only one shared filter should exist");
        assert_eq!(manager.get_ref_count(&hash1), 2, "Ref count should be 2");
    }
}