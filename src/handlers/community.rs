use nostr_sdk::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommunityId {
    pub kind: u64,
    pub pubkey: String,
    pub identifier: String,
}

impl CommunityId {
    pub fn from_a_tag(tag_value: &str) -> Option<Self> {
        // Parse 'a' tag format: "kind:pubkey:identifier"
        // Example: "34550:82341f882b6eabcd2ba7f1ef90aad961cf074af15b9ef44a09f9d2a8fbfbe6a2:rust-community"
        let parts: Vec<&str> = tag_value.split(':').collect();
        if parts.len() < 3 {
            warn!("Invalid 'a' tag format: {}", tag_value);
            return None;
        }
        
        let kind = parts[0].parse::<u64>().ok()?;
        let pubkey = parts[1].to_string();
        let identifier = parts[2..].join(":"); // Handle identifiers with colons
        
        Some(CommunityId {
            kind,
            pubkey,
            identifier,
        })
    }
    
    pub fn to_a_tag(&self) -> String {
        format!("{}:{}:{}", self.kind, self.pubkey, self.identifier)
    }
    
    pub fn to_redis_key(&self) -> String {
        format!("community:{}:{}:{}", self.kind, self.pubkey, self.identifier)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupId {
    pub relay_url: String,
    pub group_name: String,
}

impl GroupId {
    pub fn from_h_tag(tag_value: &str, relay_url: &str) -> Self {
        GroupId {
            relay_url: relay_url.to_string(),
            group_name: tag_value.to_string(),
        }
    }
    
    pub fn to_h_tag(&self) -> String {
        self.group_name.clone()
    }
    
    pub fn to_redis_key(&self) -> String {
        format!("group:{}:{}", self.relay_url, self.group_name)
    }
}

#[derive(Debug)]
pub struct CommunityHandler {
    // NIP-72 communities
    community_members: Arc<RwLock<HashMap<CommunityId, HashSet<String>>>>,
    user_communities: Arc<RwLock<HashMap<String, HashSet<CommunityId>>>>,
    
    // NIP-29 groups (for coexistence)
    group_members: Arc<RwLock<HashMap<GroupId, HashSet<String>>>>,
    user_groups: Arc<RwLock<HashMap<String, HashSet<GroupId>>>>,
}

impl Default for CommunityHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl CommunityHandler {
    pub fn new() -> Self {
        Self {
            community_members: Arc::new(RwLock::new(HashMap::new())),
            user_communities: Arc::new(RwLock::new(HashMap::new())),
            group_members: Arc::new(RwLock::new(HashMap::new())),
            user_groups: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    // NIP-72 Community methods
    pub async fn add_community_member(&self, community: CommunityId, member_pubkey: String) {
        {
            let mut community_members = self.community_members.write().await;
            community_members
                .entry(community.clone())
                .or_insert_with(HashSet::new)
                .insert(member_pubkey.clone());
        }
        
        {
            let mut user_communities = self.user_communities.write().await;
            user_communities
                .entry(member_pubkey.clone())
                .or_insert_with(HashSet::new)
                .insert(community.clone());
        }
        
        debug!("Added {} to community {}", member_pubkey, community.to_a_tag());
    }
    
    pub async fn remove_community_member(&self, community: &CommunityId, member_pubkey: &str) {
        {
            let mut community_members = self.community_members.write().await;
            if let Some(members) = community_members.get_mut(community) {
                members.remove(member_pubkey);
                if members.is_empty() {
                    community_members.remove(community);
                }
            }
        }
        
        {
            let mut user_communities = self.user_communities.write().await;
            if let Some(communities) = user_communities.get_mut(member_pubkey) {
                communities.remove(community);
                if communities.is_empty() {
                    user_communities.remove(member_pubkey);
                }
            }
        }
        
        debug!("Removed {} from community {}", member_pubkey, community.to_a_tag());
    }
    
    pub async fn get_community_members(&self, community: &CommunityId) -> HashSet<String> {
        let community_members = self.community_members.read().await;
        community_members
            .get(community)
            .cloned()
            .unwrap_or_default()
    }
    
    pub async fn get_user_communities(&self, pubkey: &str) -> HashSet<CommunityId> {
        let user_communities = self.user_communities.read().await;
        user_communities
            .get(pubkey)
            .cloned()
            .unwrap_or_default()
    }
    
    // NIP-29 Group methods
    pub async fn add_group_member(&self, group: GroupId, member_pubkey: String) {
        {
            let mut group_members = self.group_members.write().await;
            group_members
                .entry(group.clone())
                .or_insert_with(HashSet::new)
                .insert(member_pubkey.clone());
        }
        
        {
            let mut user_groups = self.user_groups.write().await;
            user_groups
                .entry(member_pubkey.clone())
                .or_insert_with(HashSet::new)
                .insert(group.clone());
        }
        
        debug!("Added {} to group {}", member_pubkey, group.to_h_tag());
    }
    
    pub async fn get_group_members(&self, group: &GroupId) -> HashSet<String> {
        let group_members = self.group_members.read().await;
        group_members
            .get(group)
            .cloned()
            .unwrap_or_default()
    }
    
    // Tag extraction methods
    pub fn extract_a_tags(event: &Event) -> Vec<String> {
        event.tags
            .iter()
            .filter_map(|tag| {
                if tag.kind() == TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::A)) {
                    tag.content().map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect()
    }
    
    pub fn extract_h_tags(event: &Event) -> Vec<String> {
        event.tags
            .iter()
            .filter_map(|tag| {
                if tag.kind() == TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)) {
                    tag.content().map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect()
    }
    
    // Event routing
    pub async fn route_event(&self, event: &Event, relay_url: &str) -> HashSet<String> {
        let mut recipients = HashSet::new();
        
        // Process 'a' tags for NIP-72 communities
        let a_tags = Self::extract_a_tags(event);
        for a_tag in a_tags {
            if let Some(community) = CommunityId::from_a_tag(&a_tag) {
                let members = self.get_community_members(&community).await;
                recipients.extend(members);
                debug!("Routing to {} members of community {}", 
                    recipients.len(), community.to_a_tag());
            }
        }
        
        // Process 'h' tags for NIP-29 groups
        let h_tags = Self::extract_h_tags(event);
        for h_tag in h_tags {
            let group = GroupId::from_h_tag(&h_tag, relay_url);
            let members = self.get_group_members(&group).await;
            recipients.extend(members);
            debug!("Routing to {} members of group {}", 
                recipients.len(), group.to_h_tag());
        }
        
        recipients
    }
    
    pub async fn cleanup_user(&self, pubkey: &str) {
        // Remove from all communities
        let communities = self.get_user_communities(pubkey).await;
        for community in communities {
            self.remove_community_member(&community, pubkey).await;
        }
        
        // Remove from all groups
        {
            let mut user_groups = self.user_groups.write().await;
            if let Some(groups) = user_groups.remove(pubkey) {
                let mut group_members = self.group_members.write().await;
                for group in groups {
                    if let Some(members) = group_members.get_mut(&group) {
                        members.remove(pubkey);
                        if members.is_empty() {
                            group_members.remove(&group);
                        }
                    }
                }
            }
        }
        
        info!("Cleaned up all community/group memberships for {}", pubkey);
    }
    
    pub async fn get_stats(&self) -> (usize, usize, usize, usize) {
        let communities = self.community_members.read().await;
        let groups = self.group_members.read().await;
        let users_in_communities = self.user_communities.read().await;
        let users_in_groups = self.user_groups.read().await;
        
        (
            communities.len(),
            groups.len(),
            users_in_communities.len(),
            users_in_groups.len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_community_id_parsing() {
        let tag = "34550:82341f882b6eabcd2ba7f1ef90aad961cf074af15b9ef44a09f9d2a8fbfbe6a2:rust-community";
        let community = CommunityId::from_a_tag(tag).unwrap();
        
        assert_eq!(community.kind, 34550);
        assert_eq!(community.pubkey, "82341f882b6eabcd2ba7f1ef90aad961cf074af15b9ef44a09f9d2a8fbfbe6a2");
        assert_eq!(community.identifier, "rust-community");
        assert_eq!(community.to_a_tag(), tag);
    }
    
    #[test]
    fn test_community_id_with_colon_in_identifier() {
        let tag = "34550:pubkey:community:with:colons";
        let community = CommunityId::from_a_tag(tag).unwrap();
        
        assert_eq!(community.identifier, "community:with:colons");
        assert_eq!(community.to_a_tag(), tag);
    }
    
    #[tokio::test]
    async fn test_add_and_remove_community_member() {
        let handler = CommunityHandler::new();
        let community = CommunityId {
            kind: 34550,
            pubkey: "creator".to_string(),
            identifier: "test-community".to_string(),
        };
        
        handler.add_community_member(community.clone(), "alice".to_string()).await;
        handler.add_community_member(community.clone(), "bob".to_string()).await;
        
        let members = handler.get_community_members(&community).await;
        assert_eq!(members.len(), 2);
        assert!(members.contains("alice"));
        assert!(members.contains("bob"));
        
        handler.remove_community_member(&community, "alice").await;
        
        let members = handler.get_community_members(&community).await;
        assert_eq!(members.len(), 1);
        assert!(!members.contains("alice"));
        assert!(members.contains("bob"));
    }
    
    #[tokio::test]
    async fn test_event_routing() {
        let handler = CommunityHandler::new();
        
        // Set up community
        let community = CommunityId {
            kind: 34550,
            pubkey: "creator".to_string(),
            identifier: "test".to_string(),
        };
        handler.add_community_member(community.clone(), "alice".to_string()).await;
        handler.add_community_member(community.clone(), "bob".to_string()).await;
        
        // Set up group
        let group = GroupId::from_h_tag("developers", "wss://relay.example.com");
        handler.add_group_member(group.clone(), "charlie".to_string()).await;
        
        // Create event with both tags
        let keys = Keys::generate();
        let event = EventBuilder::text_note("Test")
            .tags([
                Tag::custom(
                    TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::A)),
                    vec![community.to_a_tag()]
                ),
                Tag::custom(
                    TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)),
                    vec!["developers".to_string()]
                ),
            ])
            .sign_with_keys(&keys)
            .unwrap();
        
        let recipients = handler.route_event(&event, "wss://relay.example.com").await;
        assert_eq!(recipients.len(), 3);
        assert!(recipients.contains("alice"));
        assert!(recipients.contains("bob"));
        assert!(recipients.contains("charlie"));
    }
}