use nostr_sdk::prelude::*;
use serde_json::json;
use std::collections::HashSet;

#[derive(Debug, Clone)]
struct DmHandler {
    dm_kinds: Vec<u64>,
    global_subscribers: HashSet<String>, // Users who want DM notifications
}

impl DmHandler {
    fn new(dm_kinds: Vec<u64>) -> Self {
        Self {
            dm_kinds,
            global_subscribers: HashSet::new(),
        }
    }
    
    fn is_dm_event(&self, event: &Event) -> bool {
        let kind_u64 = event.kind.as_u16() as u64;
        self.dm_kinds.contains(&kind_u64)
    }
    
    fn add_dm_subscriber(&mut self, pubkey: String) {
        self.global_subscribers.insert(pubkey);
    }
    
    fn remove_dm_subscriber(&mut self, pubkey: &str) {
        self.global_subscribers.remove(pubkey);
    }
    
    fn generate_notification(&self, event: &Event) -> NotificationPayload {
        let kind_u64 = event.kind.as_u16() as u64;
        
        let (title, body) = match kind_u64 {
            1059 => ("New encrypted message", "You have received a NIP-44 encrypted message"),
            14 => ("New private message", "You have received a NIP-17 private message"),
            _ => ("New message", "You have received a new message"),
        };
        
        NotificationPayload {
            title: title.to_string(),
            body: body.to_string(),
            event_id: event.id.to_string(),
            event_kind: kind_u64,
            sender_pubkey: None, // Can't determine sender for encrypted DMs
            content: None, // Can't decrypt content
            metadata: json!({
                "encrypted": true,
                "dm_type": match kind_u64 {
                    1059 => "nip44",
                    14 => "nip17",
                    _ => "unknown"
                }
            }),
        }
    }
    
    fn route_dm_event(&self, event: &Event) -> HashSet<String> {
        if !self.is_dm_event(event) {
            return HashSet::new();
        }
        
        // For DMs, we route to all subscribers since we can't decrypt
        // to determine the actual recipient
        self.global_subscribers.clone()
    }
    
    fn extract_p_tags(event: &Event) -> Vec<String> {
        event.tags
            .iter()
            .filter_map(|tag| {
                if tag.kind() == TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::P)) {
                    tag.content().map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect()
    }
    
    fn should_process_dm(&self, event: &Event, user_pubkey: &str) -> bool {
        if !self.is_dm_event(event) {
            return false;
        }
        
        // Check if user is a global DM subscriber
        if !self.global_subscribers.contains(user_pubkey) {
            return false;
        }
        
        // For NIP-17 (kind 14), check timestamp randomization
        // Events can be up to 2 days in the past
        if event.kind.as_u16() == 14 {
            let now = Timestamp::now();
            let two_days_ago = now - std::time::Duration::from_secs(2 * 24 * 60 * 60);
            let two_days_future = now + std::time::Duration::from_secs(2 * 24 * 60 * 60);
            
            if event.created_at < two_days_ago || event.created_at > two_days_future {
                return false; // Timestamp outside acceptable range
            }
        }
        
        true
    }
}

#[derive(Debug, Clone)]
struct NotificationPayload {
    title: String,
    body: String,
    event_id: String,
    event_kind: u64,
    sender_pubkey: Option<String>,
    content: Option<String>,
    metadata: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_dm_event(kind: u64) -> Event {
        let keys = Keys::generate();
        let content = if kind == 1059 {
            // NIP-44 encrypted content (mock)
            "encrypted_payload_base64_here"
        } else if kind == 14 {
            // NIP-17 encrypted content (mock)
            "sealed_rumor_content_here"
        } else {
            "test message"
        };
        
        EventBuilder::new(Kind::from(kind as u16), content)
            .sign_with_keys(&keys)
            .unwrap()
    }
    
    fn create_dm_event_with_p_tag(kind: u64, recipient_pubkey: &str) -> Event {
        let keys = Keys::generate();
        let content = "encrypted_content";
        
        EventBuilder::new(Kind::from(kind as u16), content)
            .tag(Tag::public_key(PublicKey::from_hex(recipient_pubkey).unwrap()))
            .sign_with_keys(&keys)
            .unwrap()
    }

    #[test]
    fn test_identify_dm_event_kinds() {
        let handler = DmHandler::new(vec![1059, 14]);
        
        // Test NIP-44 DM (kind 1059)
        let nip44_event = create_dm_event(1059);
        assert!(handler.is_dm_event(&nip44_event));
        
        // Test NIP-17 DM (kind 14)
        let nip17_event = create_dm_event(14);
        assert!(handler.is_dm_event(&nip17_event));
        
        // Test regular text note (kind 1)
        let text_event = create_dm_event(1);
        assert!(!handler.is_dm_event(&text_event));
        
        // Test reaction (kind 7)
        let reaction_event = create_dm_event(7);
        assert!(!handler.is_dm_event(&reaction_event));
    }

    #[test]
    fn test_generic_notification_generation() {
        let handler = DmHandler::new(vec![1059, 14]);
        
        // Test NIP-44 notification
        let nip44_event = create_dm_event(1059);
        let nip44_notif = handler.generate_notification(&nip44_event);
        assert_eq!(nip44_notif.title, "New encrypted message");
        assert_eq!(nip44_notif.body, "You have received a NIP-44 encrypted message");
        assert_eq!(nip44_notif.event_kind, 1059);
        assert!(nip44_notif.sender_pubkey.is_none());
        assert!(nip44_notif.content.is_none());
        assert_eq!(nip44_notif.metadata["dm_type"], "nip44");
        
        // Test NIP-17 notification
        let nip17_event = create_dm_event(14);
        let nip17_notif = handler.generate_notification(&nip17_event);
        assert_eq!(nip17_notif.title, "New private message");
        assert_eq!(nip17_notif.body, "You have received a NIP-17 private message");
        assert_eq!(nip17_notif.event_kind, 14);
        assert!(nip17_notif.sender_pubkey.is_none());
        assert!(nip17_notif.content.is_none());
        assert_eq!(nip17_notif.metadata["dm_type"], "nip17");
    }

    #[test]
    fn test_encrypted_content_handling() {
        let handler = DmHandler::new(vec![1059, 14]);
        
        // Create event with encrypted content
        let event = create_dm_event(1059);
        
        // Verify we don't attempt to decrypt
        let notification = handler.generate_notification(&event);
        assert!(notification.content.is_none(), "Content should remain None for encrypted events");
        assert!(notification.sender_pubkey.is_none(), "Sender should remain None for encrypted events");
        assert_eq!(notification.metadata["encrypted"], true);
    }

    #[test]
    fn test_dm_routing_without_decryption() {
        let mut handler = DmHandler::new(vec![1059, 14]);
        
        // Add DM subscribers
        handler.add_dm_subscriber("alice".to_string());
        handler.add_dm_subscriber("bob".to_string());
        handler.add_dm_subscriber("charlie".to_string());
        
        // Create DM event
        let dm_event = create_dm_event(1059);
        
        // Route to all subscribers (can't determine actual recipient)
        let recipients = handler.route_dm_event(&dm_event);
        assert_eq!(recipients.len(), 3);
        assert!(recipients.contains("alice"));
        assert!(recipients.contains("bob"));
        assert!(recipients.contains("charlie"));
        
        // Remove a subscriber
        handler.remove_dm_subscriber("bob");
        
        // Route again
        let recipients = handler.route_dm_event(&dm_event);
        assert_eq!(recipients.len(), 2);
        assert!(recipients.contains("alice"));
        assert!(!recipients.contains("bob"));
        assert!(recipients.contains("charlie"));
    }

    #[test]
    fn test_non_dm_event_not_routed() {
        let mut handler = DmHandler::new(vec![1059, 14]);
        
        // Add DM subscribers
        handler.add_dm_subscriber("alice".to_string());
        handler.add_dm_subscriber("bob".to_string());
        
        // Create non-DM event (kind 1)
        let text_event = create_dm_event(1);
        
        // Should not route to anyone
        let recipients = handler.route_dm_event(&text_event);
        assert_eq!(recipients.len(), 0);
    }

    #[test]
    fn test_p_tag_extraction() {
        let recipient = "82341f882b6eabcd2ba7f1ef90aad961cf074af15b9ef44a09f9d2a8fbfbe6a2";
        let event = create_dm_event_with_p_tag(1059, recipient);
        
        let p_tags = DmHandler::extract_p_tags(&event);
        assert_eq!(p_tags.len(), 1);
        assert_eq!(p_tags[0], recipient);
    }

    #[test]
    fn test_nip17_timestamp_validation() {
        let mut handler = DmHandler::new(vec![1059, 14]);
        handler.add_dm_subscriber("alice".to_string());
        
        // Create NIP-17 event with current timestamp
        let current_event = create_dm_event(14);
        assert!(handler.should_process_dm(&current_event, "alice"));
        
        // Create event with timestamp 3 days in past (should be rejected)
        let keys = Keys::generate();
        let old_timestamp = Timestamp::now() - std::time::Duration::from_secs(3 * 24 * 60 * 60);
        let old_event = EventBuilder::new(Kind::from(14), "content")
            .custom_created_at(old_timestamp)
            .sign_with_keys(&keys)
            .unwrap();
        assert!(!handler.should_process_dm(&old_event, "alice"));
        
        // Create event with timestamp 1 day in past (should be accepted)
        let valid_old_timestamp = Timestamp::now() - std::time::Duration::from_secs(24 * 60 * 60);
        let valid_old_event = EventBuilder::new(Kind::from(14), "content")
            .custom_created_at(valid_old_timestamp)
            .sign_with_keys(&keys)
            .unwrap();
        assert!(handler.should_process_dm(&valid_old_event, "alice"));
    }

    #[test]
    fn test_dm_subscriber_management() {
        let mut handler = DmHandler::new(vec![1059, 14]);
        
        // Initially no subscribers
        assert_eq!(handler.global_subscribers.len(), 0);
        
        // Add subscribers
        handler.add_dm_subscriber("alice".to_string());
        handler.add_dm_subscriber("bob".to_string());
        assert_eq!(handler.global_subscribers.len(), 2);
        
        // Adding duplicate should not increase count
        handler.add_dm_subscriber("alice".to_string());
        assert_eq!(handler.global_subscribers.len(), 2);
        
        // Remove subscriber
        handler.remove_dm_subscriber("alice");
        assert_eq!(handler.global_subscribers.len(), 1);
        assert!(!handler.global_subscribers.contains("alice"));
        assert!(handler.global_subscribers.contains("bob"));
        
        // Remove non-existent subscriber
        handler.remove_dm_subscriber("charlie");
        assert_eq!(handler.global_subscribers.len(), 1);
    }
}