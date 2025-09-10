use blake3;
use nostr_sdk::prelude::*;
use serde_json::{json, Value};

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
    
    if let Some(tags) = normalized.get_mut("#e") {
        if let Some(e_array) = tags.as_array_mut() {
            e_array.sort_by(|a, b| {
                a.as_str().unwrap_or("").cmp(&b.as_str().unwrap_or(""))
            });
            e_array.dedup();
        }
    }
    
    if let Some(tags) = normalized.get_mut("#p") {
        if let Some(p_array) = tags.as_array_mut() {
            p_array.sort_by(|a, b| {
                a.as_str().unwrap_or("").cmp(&b.as_str().unwrap_or(""))
            });
            p_array.dedup();
        }
    }
    
    json!(normalized)
}

fn compute_filter_hash(filter: &Filter) -> String {
    let normalized_json = normalize_filter(filter);
    let serialized = serde_json::to_string(&normalized_json).unwrap();
    
    let hash = blake3::hash(serialized.as_bytes());
    hash.to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_filter_normalization_strips_limit() {
        let filter1 = Filter::new()
            .kinds([Kind::from(1)])
            .limit(100);
            
        let filter2 = Filter::new()
            .kinds([Kind::from(1)]);
        
        let normalized1 = normalize_filter(&filter1);
        let normalized2 = normalize_filter(&filter2);
        
        assert_eq!(normalized1, normalized2, "Filters should be equal after stripping limit");
        
        let hash1 = compute_filter_hash(&filter1);
        let hash2 = compute_filter_hash(&filter2);
        
        assert_eq!(hash1, hash2, "Filter hashes should be equal after normalization");
    }

    #[test]
    fn test_filter_normalization_strips_since() {
        let since_timestamp = Timestamp::now() - Duration::from_secs(3600);
        
        let filter1 = Filter::new()
            .kinds([Kind::from(1)])
            .since(since_timestamp);
            
        let filter2 = Filter::new()
            .kinds([Kind::from(1)]);
        
        let normalized1 = normalize_filter(&filter1);
        let normalized2 = normalize_filter(&filter2);
        
        assert_eq!(normalized1, normalized2, "Filters should be equal after stripping since");
        
        let hash1 = compute_filter_hash(&filter1);
        let hash2 = compute_filter_hash(&filter2);
        
        assert_eq!(hash1, hash2, "Filter hashes should be equal after normalization");
    }

    #[test]
    fn test_filter_normalization_strips_until() {
        let until_timestamp = Timestamp::now() + Duration::from_secs(3600);
        
        let filter1 = Filter::new()
            .kinds([Kind::from(1)])
            .until(until_timestamp);
            
        let filter2 = Filter::new()
            .kinds([Kind::from(1)]);
        
        let normalized1 = normalize_filter(&filter1);
        let normalized2 = normalize_filter(&filter2);
        
        assert_eq!(normalized1, normalized2, "Filters should be equal after stripping until");
        
        let hash1 = compute_filter_hash(&filter1);
        let hash2 = compute_filter_hash(&filter2);
        
        assert_eq!(hash1, hash2, "Filter hashes should be equal after normalization");
    }

    #[test]
    fn test_filter_normalization_preserves_other_fields() {
        let author = PublicKey::from_hex("82341f882b6eabcd2ba7f1ef90aad961cf074af15b9ef44a09f9d2a8fbfbe6a2").unwrap();
        
        let filter = Filter::new()
            .kinds([Kind::from(1), Kind::from(3)])
            .author(author)
            .custom_tag(SingleLetterTag::lowercase(Alphabet::E), "test-event-id")
            .limit(50)
            .since(Timestamp::now() - Duration::from_secs(3600));
        
        let normalized = normalize_filter(&filter);
        
        assert!(normalized.get("limit").is_none(), "limit should be removed");
        assert!(normalized.get("since").is_none(), "since should be removed");
        assert!(normalized.get("until").is_none(), "until should be removed");
        
        assert!(normalized.get("kinds").is_some(), "kinds should be preserved");
        assert!(normalized.get("authors").is_some(), "authors should be preserved");
        assert!(normalized.get("#e").is_some(), "tags should be preserved");
        
        let kinds = normalized.get("kinds").unwrap().as_array().unwrap();
        assert_eq!(kinds.len(), 2, "Should have 2 kinds");
        assert_eq!(kinds[0].as_u64().unwrap(), 1, "First kind should be 1 (sorted)");
        assert_eq!(kinds[1].as_u64().unwrap(), 3, "Second kind should be 3 (sorted)");
    }

    #[test]
    fn test_filter_normalization_sorts_and_deduplicates() {
        let author1 = PublicKey::from_hex("82341f882b6eabcd2ba7f1ef90aad961cf074af15b9ef44a09f9d2a8fbfbe6a2").unwrap();
        let author2 = PublicKey::from_hex("a2341f882b6eabcd2ba7f1ef90aad961cf074af15b9ef44a09f9d2a8fbfbe6a2").unwrap();
        
        let filter1 = Filter::new()
            .kinds([Kind::from(3), Kind::from(1), Kind::from(3)])
            .authors([author2, author1, author1]);
            
        let filter2 = Filter::new()
            .kinds([Kind::from(1), Kind::from(3)])
            .authors([author1, author2]);
        
        let hash1 = compute_filter_hash(&filter1);
        let hash2 = compute_filter_hash(&filter2);
        
        assert_eq!(hash1, hash2, "Filters with same content but different order should have same hash");
    }

    #[test]
    fn test_filter_hash_stability() {
        let filter = Filter::new()
            .kinds([Kind::from(1)])
            .custom_tag(SingleLetterTag::lowercase(Alphabet::P), "test-pubkey");
        
        let hash1 = compute_filter_hash(&filter);
        let hash2 = compute_filter_hash(&filter);
        
        assert_eq!(hash1, hash2, "Same filter should always produce same hash");
        assert_eq!(hash1.len(), 64, "SHA256 hash should be 64 hex characters");
    }
}