//! Nostr mention parsing and profile resolution for push notifications.
//!
//! This module extracts `nostr:npub...` mentions from message content,
//! fetches user profiles from Nostr relays, and replaces mentions with
//! readable names for push notification display.

use crate::error::{Result, ServiceError};
use bb8_redis::bb8::Pool;
use bb8_redis::RedisConnectionManager;
use nostr_sdk::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, warn};

/// Profile metadata from kind 0 events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileMetadata {
    pub pubkey: String,
    pub name: Option<String>,
    pub display_name: Option<String>,
}

/// Extract `nostr:npub...` mentions from text.
///
/// Returns lowercase-normalized npub identifiers.
///
/// # Examples
///
/// ```no_run
/// # use nostr_push_service::services::mention_parser::extract_npub_mentions;
/// let content = "Hey nostr:npub1abc...!";
/// let mentions = extract_npub_mentions(content);
/// assert_eq!(mentions.len(), 1);
/// ```
pub fn extract_npub_mentions(content: impl AsRef<str>) -> Vec<String> {
    let content = content.as_ref();
    let re = Regex::new(r"(?i)nostr:(npub[a-z0-9]{58,60})").unwrap();

    re.captures_iter(content)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_lowercase()))
        .collect()
}

/// Decode NIP-19 npub to hex pubkey.
///
/// # Errors
///
/// Returns `ServiceError` if npub format is invalid.
pub fn npub_to_pubkey(npub: &str) -> Result<String> {
    PublicKey::from_bech32(npub)
        .map(|pk| pk.to_hex())
        .map_err(|e| ServiceError::InvalidInput(format!("Invalid npub: {}", e)))
}

/// Service for profile fetching with Redis caching.
pub struct MentionParserService {
    redis_pool: Pool<RedisConnectionManager>,
    nostr_client: Arc<Client>,
    cache_ttl_secs: u64,
}

impl MentionParserService {
    /// Create service with Nostr client and cache TTL.
    pub fn new(
        redis_pool: Pool<RedisConnectionManager>,
        nostr_client: Arc<Client>,
        cache_ttl_secs: u64,
    ) -> Self {
        Self {
            redis_pool,
            nostr_client,
            cache_ttl_secs,
        }
    }

    /// Format content by replacing npub mentions with friendly names.
    ///
    /// Extracts mentions, fetches profiles (with caching), and replaces
    /// `nostr:npub...` with `@name` or `@display_name`. Falls back to
    /// truncated npub if profile not found.
    ///
    /// # Errors
    ///
    /// Returns error if Redis or relay connection fails critically.
    /// Individual profile fetch failures are handled gracefully.
    pub async fn format_content_for_push(&self, content: impl AsRef<str>) -> Result<String> {
        let content = content.as_ref();

        if content.is_empty() {
            return Ok(String::new());
        }

        let npub_mentions = extract_npub_mentions(content);
        if npub_mentions.is_empty() {
            return Ok(content.to_string());
        }

        // Convert npubs to pubkeys
        let mut npub_to_pubkey_map: HashMap<String, String> = HashMap::new();
        for npub in &npub_mentions {
            if let Ok(pubkey) = npub_to_pubkey(npub) {
                npub_to_pubkey_map.insert(npub.clone(), pubkey);
            }
        }

        // Fetch profiles in batch
        let pubkeys: Vec<String> = npub_to_pubkey_map.values().cloned().collect();
        let profiles = self.fetch_profiles_batch(&pubkeys).await?;

        // Build replacement map
        let mut replacements: HashMap<String, String> = HashMap::new();
        for (npub, pubkey) in &npub_to_pubkey_map {
            let friendly_name = if let Some(profile) = profiles.get(pubkey) {
                profile
                    .display_name
                    .clone()
                    .or_else(|| profile.name.clone())
                    .unwrap_or_else(|| truncate_npub(npub))
            } else {
                truncate_npub(npub)
            };

            replacements.insert(npub.clone(), format!("@{}", friendly_name));
        }

        // Replace mentions case-insensitively
        let re = Regex::new(r"(?i)nostr:(npub[a-z0-9]{58,60})").unwrap();
        let mut result = content.to_string();

        let matches: Vec<(String, usize, usize)> = re
            .captures_iter(&result)
            .map(|cap| {
                let full_match = cap.get(0).unwrap();
                let npub = cap.get(1).unwrap().as_str().to_lowercase();
                (npub, full_match.start(), full_match.end())
            })
            .collect();

        // Replace from end to start to maintain correct indices
        for (npub, start, end) in matches.iter().rev() {
            if let Some(replacement) = replacements.get(npub) {
                result.replace_range(start..end, replacement);
            }
        }

        Ok(result)
    }

    /// Fetch profiles in batch with Redis caching.
    ///
    /// # Errors
    ///
    /// Returns error only for critical failures. Individual profile
    /// fetch failures are logged and skipped.
    async fn fetch_profiles_batch(
        &self,
        pubkeys: &[String],
    ) -> Result<HashMap<String, ProfileMetadata>> {
        if pubkeys.is_empty() {
            return Ok(HashMap::new());
        }

        let mut profiles = HashMap::new();
        let mut missing_pubkeys = Vec::new();

        // Check cache first
        let mut conn = self.redis_pool.get().await.map_err(|e| {
            ServiceError::Internal(format!("Failed to get Redis connection: {}", e))
        })?;

        for pubkey in pubkeys {
            let cache_key = format!("profile_cache:{}", pubkey);
            let cached_json: Option<String> = redis::cmd("GET")
                .arg(&cache_key)
                .query_async(&mut *conn)
                .await
                .unwrap_or(None);
            
            match cached_json {
                Some(json) => {
                    if let Ok(profile) = serde_json::from_str::<ProfileMetadata>(&json) {
                        debug!(pubkey = %pubkey, "Profile cache hit");
                        profiles.insert(pubkey.clone(), profile);
                    } else {
                        missing_pubkeys.push(pubkey.clone());
                    }
                }
                None => {
                    missing_pubkeys.push(pubkey.clone());
                }
            }
        }

        // Fetch missing profiles from relays
        if !missing_pubkeys.is_empty() {
            match self.fetch_from_relays(&missing_pubkeys).await {
                Ok(fetched) => {
                    // Cache fetched profiles
                    for (pubkey, profile) in &fetched {
                        if let Ok(json) = serde_json::to_string(profile) {
                            let cache_key = format!("profile_cache:{}", pubkey);
                            let _: Result<(), redis::RedisError> = redis::cmd("SETEX")
                                .arg(&cache_key)
                                .arg(self.cache_ttl_secs)
                                .arg(json)
                                .query_async(&mut *conn)
                                .await;
                        }
                        profiles.insert(pubkey.clone(), profile.clone());
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to fetch profiles from relays");
                }
            }
        }

        Ok(profiles)
    }

    /// Fetch profiles from Nostr relays using the shared client.
    async fn fetch_from_relays(
        &self,
        pubkeys: &[String],
    ) -> Result<HashMap<String, ProfileMetadata>> {
        // Parse pubkeys
        let public_keys: Vec<PublicKey> = pubkeys
            .iter()
            .filter_map(|pk| PublicKey::from_hex(pk).ok())
            .collect();

        if public_keys.is_empty() {
            return Ok(HashMap::new());
        }

        // Query kind 0 events using the shared client
        let filter = Filter::new()
            .kind(Kind::Metadata)
            .authors(public_keys)
            .limit(pubkeys.len());

        let timeout = std::time::Duration::from_secs(5);
        let events = self.nostr_client
            .fetch_events(filter, timeout)
            .await
            .map_err(|e| ServiceError::RelayError(format!("Failed to fetch events: {}", e)))?;

        // Parse profiles
        let mut profiles = HashMap::new();
        for event in events {
            if let Ok(metadata) = serde_json::from_str::<Metadata>(&event.content) {
                profiles.insert(
                    event.pubkey.to_hex(),
                    ProfileMetadata {
                        pubkey: event.pubkey.to_hex(),
                        name: metadata.name,
                        display_name: metadata.display_name,
                    },
                );
            }
        }

        Ok(profiles)
    }
}

/// Truncate npub to readable format.
///
/// Returns `npub1xxxx...xxxxxx` (first 10 + last 6 chars).
fn truncate_npub(npub: &str) -> String {
    if npub.len() >= 16 {
        format!("{}...{}", &npub[..10], &npub[npub.len() - 6..])
    } else {
        npub.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_npub_mentions() {
        let content = "Hello nostr:npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6!";
        let mentions = extract_npub_mentions(content);
        assert_eq!(mentions.len(), 1);
        assert_eq!(
            mentions[0],
            "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6"
        );
    }

    #[test]
    fn test_extract_multiple_mentions() {
        let content = "Hey nostr:npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6 and nostr:npub1l2vyh47mk2p0qlsku7hg0vn29faehy9hy34ygaclpn66ukqp3afqutajft check this!";
        let mentions = extract_npub_mentions(content);
        assert_eq!(mentions.len(), 2);
    }

    #[test]
    fn test_extract_case_insensitive() {
        let content = "NOSTR:NPUB180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6";
        let mentions = extract_npub_mentions(content);
        assert_eq!(mentions.len(), 1);
    }

    #[test]
    fn test_npub_to_pubkey_valid() {
        let npub = "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6";
        let result = npub_to_pubkey(npub);
        assert!(result.is_ok());
        let pubkey = result.unwrap();
        assert_eq!(pubkey.len(), 64);
    }

    #[test]
    fn test_npub_to_pubkey_invalid() {
        let result = npub_to_pubkey("invalid_npub");
        assert!(result.is_err());
    }

    #[test]
    fn test_truncate_npub() {
        let npub = "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6";
        let truncated = truncate_npub(npub);
        assert!(truncated.starts_with("npub180cvv"));
        assert!(truncated.ends_with("jh6w6"));
        assert!(truncated.contains("..."));
    }
}
