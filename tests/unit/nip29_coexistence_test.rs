use nostr_sdk::prelude::*;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GroupId {
    relay_url: String,
    group_name: String,
}

impl GroupId {
    fn from_h_tag(tag_value: &str, relay_url: &str) -> Self {
        GroupId {
            relay_url: relay_url.to_string(),
            group_name: tag_value.to_string(),
        }
    }
    
    fn to_h_tag(&self) -> String {
        self.group_name.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CommunityId {
    kind: u64,
    pubkey: String,
    identifier: String,
}

impl CommunityId {
    fn from_a_tag(tag_value: &str) -> Option<Self> {
        let parts: Vec<&str> = tag_value.split(':').collect();
        if parts.len() != 3 {
            return None;
        }
        
        let kind = parts[0].parse::<u64>().ok()?;
        let pubkey = parts[1].to_string();
        let identifier = parts[2].to_string();
        
        Some(CommunityId {
            kind,
            pubkey,
            identifier,
        })
    }
    
    fn to_a_tag(&self) -> String {
        format!("{}:{}:{}", self.kind, self.pubkey, self.identifier)
    }
}

#[derive(Debug)]
struct UnifiedRouter {
    // NIP-29 groups
    group_members: HashMap<GroupId, HashSet<String>>,
    user_groups: HashMap<String, HashSet<GroupId>>,
    
    // NIP-72 communities
    community_members: HashMap<CommunityId, HashSet<String>>,
    user_communities: HashMap<String, HashSet<CommunityId>>,
}

impl UnifiedRouter {
    fn new() -> Self {
        Self {
            group_members: HashMap::new(),
            user_groups: HashMap::new(),
            community_members: HashMap::new(),
            user_communities: HashMap::new(),
        }
    }
    
    // NIP-29 group methods
    fn add_group_member(&mut self, group: GroupId, member_pubkey: String) {
        self.group_members
            .entry(group.clone())
            .or_insert_with(HashSet::new)
            .insert(member_pubkey.clone());
            
        self.user_groups
            .entry(member_pubkey)
            .or_insert_with(HashSet::new)
            .insert(group);
    }
    
    fn get_group_members(&self, group: &GroupId) -> HashSet<String> {
        self.group_members
            .get(group)
            .cloned()
            .unwrap_or_default()
    }
    
    // NIP-72 community methods
    fn add_community_member(&mut self, community: CommunityId, member_pubkey: String) {
        self.community_members
            .entry(community.clone())
            .or_insert_with(HashSet::new)
            .insert(member_pubkey.clone());
            
        self.user_communities
            .entry(member_pubkey)
            .or_insert_with(HashSet::new)
            .insert(community);
    }
    
    fn get_community_members(&self, community: &CommunityId) -> HashSet<String> {
        self.community_members
            .get(community)
            .cloned()
            .unwrap_or_default()
    }
    
    fn extract_h_tags(event: &Event) -> Vec<String> {
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
    
    fn extract_a_tags(event: &Event) -> Vec<String> {
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
    
    fn route_event(&self, event: &Event, relay_url: &str) -> HashSet<String> {
        let mut recipients = HashSet::new();
        
        // Process 'h' tags for NIP-29 groups
        let h_tags = Self::extract_h_tags(event);
        for h_tag in h_tags {
            let group = GroupId::from_h_tag(&h_tag, relay_url);
            let members = self.get_group_members(&group);
            recipients.extend(members);
        }
        
        // Process 'a' tags for NIP-72 communities
        let a_tags = Self::extract_a_tags(event);
        for a_tag in a_tags {
            if let Some(community) = CommunityId::from_a_tag(&a_tag) {
                let members = self.get_community_members(&community);
                recipients.extend(members);
            }
        }
        
        recipients
    }
    
    fn get_user_subscriptions(&self, pubkey: &str) -> (HashSet<GroupId>, HashSet<CommunityId>) {
        let groups = self.user_groups
            .get(pubkey)
            .cloned()
            .unwrap_or_default();
            
        let communities = self.user_communities
            .get(pubkey)
            .cloned()
            .unwrap_or_default();
            
        (groups, communities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_event_with_tags(h_tags: Vec<&str>, a_tags: Vec<&str>) -> Event {
        let keys = Keys::generate();
        let mut tags = Vec::new();
        
        for h_tag in h_tags {
            tags.push(Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)),
                vec![h_tag.to_string()]
            ));
        }
        
        for a_tag in a_tags {
            tags.push(Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::A)),
                vec![a_tag.to_string()]
            ));
        }
        
        let builder = EventBuilder::text_note("Test message").tags(tags);
        builder.sign_with_keys(&keys).unwrap()
    }

    #[test]
    fn test_parse_h_tags() {
        let event = create_event_with_tags(
            vec!["rust-developers", "bitcoin-group"],
            vec![]
        );
        
        let h_tags = UnifiedRouter::extract_h_tags(&event);
        assert_eq!(h_tags.len(), 2);
        assert!(h_tags.contains(&"rust-developers".to_string()));
        assert!(h_tags.contains(&"bitcoin-group".to_string()));
    }
    
    #[test]
    fn test_parse_a_tags() {
        let event = create_event_with_tags(
            vec![],
            vec!["34550:pubkey1:community1", "34550:pubkey2:community2"]
        );
        
        let a_tags = UnifiedRouter::extract_a_tags(&event);
        assert_eq!(a_tags.len(), 2);
        assert!(a_tags.contains(&"34550:pubkey1:community1".to_string()));
        assert!(a_tags.contains(&"34550:pubkey2:community2".to_string()));
    }

    #[test]
    fn test_nip29_and_nip72_tags_in_same_event() {
        let event = create_event_with_tags(
            vec!["developers-group"],
            vec!["34550:creator:rust-community"]
        );
        
        let h_tags = UnifiedRouter::extract_h_tags(&event);
        let a_tags = UnifiedRouter::extract_a_tags(&event);
        
        assert_eq!(h_tags.len(), 1);
        assert_eq!(a_tags.len(), 1);
        assert!(h_tags.contains(&"developers-group".to_string()));
        assert!(a_tags.contains(&"34550:creator:rust-community".to_string()));
    }

    #[test]
    fn test_route_to_both_group_and_community_members() {
        let mut router = UnifiedRouter::new();
        let relay_url = "wss://relay.example.com";
        
        // Set up NIP-29 group
        let group = GroupId::from_h_tag("developers", relay_url);
        router.add_group_member(group.clone(), "alice".to_string());
        router.add_group_member(group.clone(), "bob".to_string());
        
        // Set up NIP-72 community
        let community = CommunityId {
            kind: 34550,
            pubkey: "creator".to_string(),
            identifier: "rust-community".to_string(),
        };
        router.add_community_member(community.clone(), "charlie".to_string());
        router.add_community_member(community.clone(), "diana".to_string());
        
        // Create event with both tags
        let event = create_event_with_tags(
            vec!["developers"],
            vec![&community.to_a_tag()]
        );
        
        // Route the event
        let recipients = router.route_event(&event, relay_url);
        
        assert_eq!(recipients.len(), 4);
        assert!(recipients.contains("alice"));
        assert!(recipients.contains("bob"));
        assert!(recipients.contains("charlie"));
        assert!(recipients.contains("diana"));
    }

    #[test]
    fn test_separate_group_and_community_management() {
        let mut router = UnifiedRouter::new();
        let relay_url = "wss://relay.example.com";
        
        // Alice is in both a group and a community
        let group = GroupId::from_h_tag("group1", relay_url);
        router.add_group_member(group.clone(), "alice".to_string());
        
        let community = CommunityId {
            kind: 34550,
            pubkey: "creator".to_string(),
            identifier: "community1".to_string(),
        };
        router.add_community_member(community.clone(), "alice".to_string());
        
        // Bob is only in the group
        router.add_group_member(group.clone(), "bob".to_string());
        
        // Charlie is only in the community
        router.add_community_member(community.clone(), "charlie".to_string());
        
        // Check Alice's subscriptions
        let (alice_groups, alice_communities) = router.get_user_subscriptions("alice");
        assert_eq!(alice_groups.len(), 1);
        assert_eq!(alice_communities.len(), 1);
        assert!(alice_groups.contains(&group));
        assert!(alice_communities.contains(&community));
        
        // Check Bob's subscriptions
        let (bob_groups, bob_communities) = router.get_user_subscriptions("bob");
        assert_eq!(bob_groups.len(), 1);
        assert_eq!(bob_communities.len(), 0);
        assert!(bob_groups.contains(&group));
        
        // Check Charlie's subscriptions
        let (charlie_groups, charlie_communities) = router.get_user_subscriptions("charlie");
        assert_eq!(charlie_groups.len(), 0);
        assert_eq!(charlie_communities.len(), 1);
        assert!(charlie_communities.contains(&community));
    }

    #[test]
    fn test_overlapping_membership() {
        let mut router = UnifiedRouter::new();
        let relay_url = "wss://relay.example.com";
        
        // Alice is in group1 and community1
        let group1 = GroupId::from_h_tag("group1", relay_url);
        router.add_group_member(group1.clone(), "alice".to_string());
        
        let community1 = CommunityId {
            kind: 34550,
            pubkey: "creator1".to_string(),
            identifier: "community1".to_string(),
        };
        router.add_community_member(community1.clone(), "alice".to_string());
        
        // Bob is in group1 and community1 (overlaps with Alice)
        router.add_group_member(group1.clone(), "bob".to_string());
        router.add_community_member(community1.clone(), "bob".to_string());
        
        // Event targeting only the group
        let group_event = create_event_with_tags(vec!["group1"], vec![]);
        let group_recipients = router.route_event(&group_event, relay_url);
        assert_eq!(group_recipients.len(), 2);
        assert!(group_recipients.contains("alice"));
        assert!(group_recipients.contains("bob"));
        
        // Event targeting only the community
        let community_event = create_event_with_tags(vec![], vec![&community1.to_a_tag()]);
        let community_recipients = router.route_event(&community_event, relay_url);
        assert_eq!(community_recipients.len(), 2);
        assert!(community_recipients.contains("alice"));
        assert!(community_recipients.contains("bob"));
        
        // Event targeting both (should still only have 2 unique recipients)
        let both_event = create_event_with_tags(
            vec!["group1"],
            vec![&community1.to_a_tag()]
        );
        let both_recipients = router.route_event(&both_event, relay_url);
        assert_eq!(both_recipients.len(), 2);
        assert!(both_recipients.contains("alice"));
        assert!(both_recipients.contains("bob"));
    }

    #[test]
    fn test_multiple_relays_same_group_name() {
        let mut router = UnifiedRouter::new();
        
        // Same group name on different relays
        let group_relay1 = GroupId::from_h_tag("developers", "wss://relay1.com");
        let group_relay2 = GroupId::from_h_tag("developers", "wss://relay2.com");
        
        // Different members for each relay's group
        router.add_group_member(group_relay1.clone(), "alice".to_string());
        router.add_group_member(group_relay2.clone(), "bob".to_string());
        
        // Event on relay1
        let event = create_event_with_tags(vec!["developers"], vec![]);
        let recipients_relay1 = router.route_event(&event, "wss://relay1.com");
        assert_eq!(recipients_relay1.len(), 1);
        assert!(recipients_relay1.contains("alice"));
        assert!(!recipients_relay1.contains("bob"));
        
        // Same event on relay2
        let recipients_relay2 = router.route_event(&event, "wss://relay2.com");
        assert_eq!(recipients_relay2.len(), 1);
        assert!(recipients_relay2.contains("bob"));
        assert!(!recipients_relay2.contains("alice"));
    }
}