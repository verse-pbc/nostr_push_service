use nostr_sdk::prelude::*;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CommunityId {
    kind: u64,
    pubkey: String,
    identifier: String,
}

impl CommunityId {
    fn from_a_tag(tag_value: &str) -> Option<Self> {
        // Parse 'a' tag format: "34550:pubkey:identifier"
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
struct CommunityRouter {
    community_members: HashMap<CommunityId, HashSet<String>>, // community -> member pubkeys
    user_communities: HashMap<String, HashSet<CommunityId>>,  // pubkey -> communities
}

impl CommunityRouter {
    fn new() -> Self {
        Self {
            community_members: HashMap::new(),
            user_communities: HashMap::new(),
        }
    }
    
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
    
    fn remove_community_member(&mut self, community: &CommunityId, member_pubkey: &str) {
        if let Some(members) = self.community_members.get_mut(community) {
            members.remove(member_pubkey);
            if members.is_empty() {
                self.community_members.remove(community);
            }
        }
        
        if let Some(communities) = self.user_communities.get_mut(member_pubkey) {
            communities.remove(community);
            if communities.is_empty() {
                self.user_communities.remove(member_pubkey);
            }
        }
    }
    
    fn get_community_members(&self, community: &CommunityId) -> HashSet<String> {
        self.community_members
            .get(community)
            .cloned()
            .unwrap_or_default()
    }
    
    fn get_user_communities(&self, pubkey: &str) -> HashSet<CommunityId> {
        self.user_communities
            .get(pubkey)
            .cloned()
            .unwrap_or_default()
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
    
    fn route_event(&self, event: &Event) -> HashSet<String> {
        let mut recipients = HashSet::new();
        
        // Extract 'a' tags from the event
        let a_tags = Self::extract_a_tags(event);
        
        // For each 'a' tag, find the community and its members
        for a_tag in a_tags {
            if let Some(community) = CommunityId::from_a_tag(&a_tag) {
                let members = self.get_community_members(&community);
                recipients.extend(members);
            }
        }
        
        recipients
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn create_test_event_with_a_tag(a_tag_value: &str) -> Event {
        let keys = Keys::generate();
        let builder = EventBuilder::text_note("Test message")
            .tags([Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::A)),
                vec![a_tag_value.to_string()]
            )]);
        
        builder.sign_with_keys(&keys).unwrap()
    }

    #[test]
    fn test_parse_a_tag() {
        let tag_value = "34550:82341f882b6eabcd2ba7f1ef90aad961cf074af15b9ef44a09f9d2a8fbfbe6a2:rust-community";
        let community = CommunityId::from_a_tag(tag_value).unwrap();
        
        assert_eq!(community.kind, 34550);
        assert_eq!(community.pubkey, "82341f882b6eabcd2ba7f1ef90aad961cf074af15b9ef44a09f9d2a8fbfbe6a2");
        assert_eq!(community.identifier, "rust-community");
        
        // Test round-trip
        assert_eq!(community.to_a_tag(), tag_value);
    }
    
    #[test]
    fn test_parse_invalid_a_tag() {
        // Missing parts
        assert!(CommunityId::from_a_tag("34550:pubkey").is_none());
        
        // Invalid kind
        assert!(CommunityId::from_a_tag("invalid:pubkey:identifier").is_none());
        
        // Empty string
        assert!(CommunityId::from_a_tag("").is_none());
    }

    #[test]
    fn test_route_event_to_community_subscribers() {
        let mut router = CommunityRouter::new();
        
        let community = CommunityId {
            kind: 34550,
            pubkey: "creator_pubkey".to_string(),
            identifier: "rust-community".to_string(),
        };
        
        // Add community members
        router.add_community_member(community.clone(), "user1".to_string());
        router.add_community_member(community.clone(), "user2".to_string());
        router.add_community_member(community.clone(), "user3".to_string());
        
        // Create event with 'a' tag pointing to the community
        let event = create_test_event_with_a_tag(&community.to_a_tag());
        
        // Route the event
        let recipients = router.route_event(&event);
        
        assert_eq!(recipients.len(), 3);
        assert!(recipients.contains("user1"));
        assert!(recipients.contains("user2"));
        assert!(recipients.contains("user3"));
    }

    #[test]
    fn test_community_member_management() {
        let mut router = CommunityRouter::new();
        
        let community = CommunityId {
            kind: 34550,
            pubkey: "creator".to_string(),
            identifier: "test-community".to_string(),
        };
        
        // Add members
        router.add_community_member(community.clone(), "alice".to_string());
        router.add_community_member(community.clone(), "bob".to_string());
        
        let members = router.get_community_members(&community);
        assert_eq!(members.len(), 2);
        assert!(members.contains("alice"));
        assert!(members.contains("bob"));
        
        // Remove a member
        router.remove_community_member(&community, "alice");
        
        let members = router.get_community_members(&community);
        assert_eq!(members.len(), 1);
        assert!(!members.contains("alice"));
        assert!(members.contains("bob"));
        
        // Check user's communities
        let alice_communities = router.get_user_communities("alice");
        assert_eq!(alice_communities.len(), 0);
        
        let bob_communities = router.get_user_communities("bob");
        assert_eq!(bob_communities.len(), 1);
        assert!(bob_communities.contains(&community));
    }

    #[test]
    fn test_multiple_communities() {
        let mut router = CommunityRouter::new();
        
        let community1 = CommunityId {
            kind: 34550,
            pubkey: "creator1".to_string(),
            identifier: "rust-community".to_string(),
        };
        
        let community2 = CommunityId {
            kind: 34550,
            pubkey: "creator2".to_string(),
            identifier: "bitcoin-community".to_string(),
        };
        
        // Alice is in both communities
        router.add_community_member(community1.clone(), "alice".to_string());
        router.add_community_member(community2.clone(), "alice".to_string());
        
        // Bob is only in community1
        router.add_community_member(community1.clone(), "bob".to_string());
        
        // Charlie is only in community2
        router.add_community_member(community2.clone(), "charlie".to_string());
        
        // Check Alice's communities
        let alice_communities = router.get_user_communities("alice");
        assert_eq!(alice_communities.len(), 2);
        assert!(alice_communities.contains(&community1));
        assert!(alice_communities.contains(&community2));
        
        // Route event to community1
        let event1 = create_test_event_with_a_tag(&community1.to_a_tag());
        let recipients1 = router.route_event(&event1);
        assert_eq!(recipients1.len(), 2);
        assert!(recipients1.contains("alice"));
        assert!(recipients1.contains("bob"));
        assert!(!recipients1.contains("charlie"));
        
        // Route event to community2
        let event2 = create_test_event_with_a_tag(&community2.to_a_tag());
        let recipients2 = router.route_event(&event2);
        assert_eq!(recipients2.len(), 2);
        assert!(recipients2.contains("alice"));
        assert!(recipients2.contains("charlie"));
        assert!(!recipients2.contains("bob"));
    }

    #[test]
    fn test_event_with_multiple_a_tags() {
        let mut router = CommunityRouter::new();
        
        let community1 = CommunityId {
            kind: 34550,
            pubkey: "creator1".to_string(),
            identifier: "community1".to_string(),
        };
        
        let community2 = CommunityId {
            kind: 34550,
            pubkey: "creator2".to_string(),
            identifier: "community2".to_string(),
        };
        
        // Set up members
        router.add_community_member(community1.clone(), "alice".to_string());
        router.add_community_member(community1.clone(), "bob".to_string());
        router.add_community_member(community2.clone(), "charlie".to_string());
        router.add_community_member(community2.clone(), "diana".to_string());
        
        // Create event with multiple 'a' tags
        let keys = Keys::generate();
        let builder = EventBuilder::text_note("Cross-posted message")
            .tags([
                Tag::custom(
                    TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::A)),
                    vec![community1.to_a_tag()]
                ),
                Tag::custom(
                    TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::A)),
                    vec![community2.to_a_tag()]
                ),
            ]);
        let event = builder.sign_with_keys(&keys).unwrap();
        
        // Route the event - should go to members of both communities
        let recipients = router.route_event(&event);
        assert_eq!(recipients.len(), 4);
        assert!(recipients.contains("alice"));
        assert!(recipients.contains("bob"));
        assert!(recipients.contains("charlie"));
        assert!(recipients.contains("diana"));
    }
    
    #[test]
    fn test_extract_a_tags_from_event() {
        let community1 = "34550:pubkey1:community1";
        let community2 = "34550:pubkey2:community2";
        
        let keys = Keys::generate();
        let builder = EventBuilder::text_note("Test")
            .tags([
                Tag::custom(
                    TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::A)),
                    vec![community1.to_string()]
                ),
                Tag::custom(
                    TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::A)),
                    vec![community2.to_string()]
                ),
                // Add a non-'a' tag to ensure it's filtered out
                Tag::custom(
                    TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::P)),
                    vec!["some_pubkey".to_string()]
                ),
            ]);
        let event = builder.sign_with_keys(&keys).unwrap();
        
        let a_tags = CommunityRouter::extract_a_tags(&event);
        assert_eq!(a_tags.len(), 2);
        assert!(a_tags.contains(&community1.to_string()));
        assert!(a_tags.contains(&community2.to_string()));
    }
}