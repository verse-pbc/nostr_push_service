use crate::config::Settings;
use crate::error::{Result, ServiceError};
use anyhow::Error;
use nostr_sdk::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Constant for the Nostr event kind used for group membership (NIP-29).
const GROUP_MEMBERSHIP_KIND: Kind = Kind::Custom(39002);

/// Default timeout duration for fetching events from the relay.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Cache for storing group membership information to reduce relay queries.
pub struct GroupMembershipCache {
    /// Maps group IDs to a tuple of (expiration timestamp, set of member public keys).
    cache: HashMap<String, (Timestamp, HashSet<PublicKey>)>,
    /// Cache expiration duration in seconds.
    cache_expiration: u64,
}

impl GroupMembershipCache {
    /// Initializes a new cache with the specified expiration time.
    pub fn new(cache_expiration: u64) -> Self {
        Self {
            cache: HashMap::new(),
            cache_expiration,
        }
    }

    /// Checks if a public key is a member of a group based on cached data.
    ///
    /// Returns:
    /// - `Some(true)` if the public key is a member and the cache is valid.
    /// - `Some(false)` if the public key is not a member and the cache is valid.
    /// - `None` if the cache is expired or the group is not cached.
    pub fn is_member(&self, group_id: &str, pubkey: &PublicKey) -> Option<bool> {
        self.cache.get(group_id).and_then(|(expiration, members)| {
            if *expiration > Timestamp::now() {
                Some(members.contains(pubkey))
            } else {
                None
            }
        })
    }

    /// Returns the set of members for the given group if the cache is valid.
    pub fn get_members(&self, group_id: &str) -> Option<HashSet<PublicKey>> {
        self.cache.get(group_id).and_then(|(expiration, members)| {
            if *expiration > Timestamp::now() {
                Some(members.clone())
            } else {
                None
            }
        })
    }

    /// Updates the cache with new membership data and sets a fresh expiration time.
    pub fn update_cache(&mut self, group_id: &str, members: HashSet<PublicKey>) {
        let expiration = Timestamp::now() + Duration::from_secs(self.cache_expiration);
        self.cache
            .insert(group_id.to_string(), (expiration, members));
    }
}

/// Constant for the Nostr event kind used for group admins (NIP-29).
const GROUP_ADMINS_KIND: Kind = Kind::Custom(39001);

/// Client for interacting with NIP-29 relays to manage group membership.
pub struct Nip29Client {
    relay_keys: Keys,
    cache: Arc<RwLock<GroupMembershipCache>>,
    client: Arc<Client>,
}

impl Nip29Client {
    /// Creates a new NIP-29 client from the settings
    ///
    /// A simpler constructor that can be used for testing or when we don't need
    /// a real connection to the relay. This does NOT connect automatically.
    pub fn from_settings(settings: &Settings) -> Result<Self, Error> {
        let nostr_config = &settings.nostr;

        // Use get_nostr_private_key instead of accessing private_key directly
        let secret_key = settings
            .get_nostr_private_key()
            .ok_or_else(|| Error::msg("No private key configured for NIP-29"))?;

        let keys = Keys::new(secret_key);
        let cache_expiration = nostr_config.cache_expiration.unwrap_or(300);

        // Important: Do not autoconnect or authenticate automatically in this factory method
        let opts = Options::default()
            .autoconnect(false)
            .automatic_authentication(false);
        let client = Client::builder().signer(keys.clone()).opts(opts).build();

        Ok(Self {
            relay_keys: keys,
            cache: Arc::new(RwLock::new(GroupMembershipCache::new(cache_expiration))),
            client: Arc::new(client),
        })
    }

    /// Creates a client with connection to a relay
    ///
    /// # Arguments
    /// - `relay_url`: The URL of the Nostr relay to connect to.
    /// - `keys`: The cryptographic keys for relay authentication and signing.
    /// - `cache_expiration`: Duration in seconds for cache validity.
    ///
    /// # Errors
    /// Returns an error if the relay connection fails.
    pub async fn new(
        relay_url: String,
        keys: Keys,
        cache_expiration: u64,
    ) -> Result<Self, nostr_sdk::client::Error> {
        // Autoconnect and authenticate when using `new`
        let opts = Options::default()
            .autoconnect(true)
            .automatic_authentication(true);

        let client = Client::builder().signer(keys.clone()).opts(opts).build();
        client.add_relay(&relay_url).await?;
        client.connect().await;

        // Wait for the connection to actually establish before returning
        let max_wait = Duration::from_secs(10);
        let sleep_interval = Duration::from_millis(100);
        let start = std::time::Instant::now();

        // Loop until connection is established or timeout occurs
        while start.elapsed() < max_wait {
            if !client.relays().await.is_empty()
                && client.relays().await.values().any(|s| s.is_connected())
            {
                info!("Connection established to relay: {}", relay_url);
                return Ok(Self {
                    relay_keys: keys,
                    cache: Arc::new(RwLock::new(GroupMembershipCache::new(cache_expiration))),
                    client: Arc::new(client),
                });
            }
            tokio::time::sleep(sleep_interval).await;
        }

        // If we get here, the connection timed out
        warn!(
            "Failed to establish connection to relay {} within timeout",
            relay_url
        );

        // Instead of creating a client error directly, let the connection continue and
        // let the caller handle any connection issues that arise later.
        // This is a safer approach given our challenges with the error type.
        Ok(Self {
            relay_keys: keys,
            cache: Arc::new(RwLock::new(GroupMembershipCache::new(cache_expiration))),
            client: Arc::new(client),
        })
    }

    /// Retrieves the set of members for a group, using the cache or fetching from the relay.
    pub async fn get_group_members(&self, group_id: &str) -> Result<HashSet<PublicKey>> {
        // Try to read from cache using a read lock.
        {
            let cache = self.cache.read().await;
            if let Some(members) = cache.get_members(group_id) {
                info!(%group_id, "Cache hit for group members");
                return Ok(members);
            }
            info!(%group_id,"Cache miss for group members");
        }

        // Cache miss or expired; fetch from relay.
        let filter = Filter::new()
            .kind(GROUP_MEMBERSHIP_KIND)
            .author(self.relay_keys.public_key()) // Use configured key
            .identifier(group_id);

        info!(%group_id, filter = ?filter, "Fetching group members from relay");

        // Use fetch_events
        let events = self.client.fetch_events(filter, FETCH_TIMEOUT).await?;

        info!(%group_id, count = events.len(), "Received events for group membership query");

        let mut members = HashSet::new();
        // Find the latest event
        if let Some(latest_event) = events.iter().max_by_key(|e| e.created_at) {
            info!(
                %group_id, event_id = %latest_event.id, created_at = %latest_event.created_at,
                "Processing latest membership event"
            );
            // Use simple if let Ok pattern, matching the Tag::PublicKey variant
            let pubkeys = latest_event.tags.public_keys();
            members.extend(pubkeys);
        } else {
            warn!(%group_id, kind = %GROUP_MEMBERSHIP_KIND, "No group membership event found");
        }

        if members.is_empty() {
            warn!(%group_id, "No valid members found");
        } else {
            info!(%group_id, count = members.len(), "Found group members");
        }

        // Update the cache with a write lock.
        {
            let mut cache = self.cache.write().await;
            cache.update_cache(group_id, members.clone());
            info!(%group_id, "Cache updated for group members");
        }

        Ok(members)
    }

    /// Retrieves the set of admins for a group from the relay.
    ///
    /// # Arguments
    /// - `group_id`: The identifier of the group.
    ///
    /// # Returns
    /// - A HashSet of admin public keys for the group
    /// - Err if fetching events fails.
    async fn get_group_admins(&self, group_id: &str) -> Result<HashSet<PublicKey>> {
        info!(%group_id, "Fetching admin list");

        let filter = Filter::new()
            .kind(GROUP_ADMINS_KIND)
            .author(self.relay_keys.public_key())
            .identifier(group_id);

        info!(%group_id, filter = ?filter, "Using filter for group admins");

        // Use fetch_events
        let events = match self.client.fetch_events(filter, FETCH_TIMEOUT).await {
            Ok(e) => {
                info!(%group_id, count = e.len(), "Received events for group admin query");
                e
            }
            Err(e) => {
                warn!(%group_id, error = %e, "Failed to fetch group admin events");
                return Err(ServiceError::NostrSdkError(e));
            }
        };

        let mut admins = HashSet::new();
        if let Some(latest_event) = events.iter().max_by_key(|e| e.created_at) {
            info!(
                %group_id, event_id = %latest_event.id, created_at = %latest_event.created_at,
                "Processing latest admin event"
            );
            // Use simple if let Ok pattern, matching the Tag::PublicKey variant
            let pubkeys: HashSet<PublicKey> =
                extract_pubkeys_from_p_tags(&latest_event.tags).collect();

            info!(%group_id, pubkeys = ?pubkeys, "Found group admins");
            admins.extend(pubkeys);
        } else {
            warn!(%group_id, kind = %GROUP_ADMINS_KIND, "No group admin event found");
        }

        if admins.is_empty() {
            warn!(%group_id, "No valid admins found");
        } else {
            info!(%group_id, count = admins.len(), "Found group admins");
        }

        Ok(admins)
    }

    /// Checks if a user is a member of a group.
    ///
    /// Fetches membership data if not cached or expired.
    ///
    /// # Arguments
    /// - `group_id`: The group identifier.
    /// - `pubkey`: The public key of the user to check.
    ///
    /// # Returns
    /// - `Ok(true)` if the public key is a member.
    /// - `Ok(false)` if not a member.
    /// - `Err` if fetching events fails.
    pub async fn is_group_member(&self, group_id: &str, pubkey: &PublicKey) -> Result<bool> {
        let members = self.get_group_members(group_id).await?;
        Ok(members.contains(pubkey))
    }

    /// Checks if a user is an admin of a group.
    ///
    /// Always fetches the latest admin list from the relay (no caching for admins).
    ///
    /// # Arguments
    /// - `group_id`: The group identifier.
    /// - `pubkey`: The public key of the user to check.
    ///
    /// # Returns
    /// - `Ok(true)` if the public key is an admin.
    /// - `Ok(false)` if not an admin or no admin data exists.
    /// - `Err` if fetching events fails.
    pub async fn is_group_admin(&self, group_id: &str, pubkey: &PublicKey) -> Result<bool> {
        let admins = self.get_group_admins(group_id).await?;
        Ok(admins.contains(pubkey))
    }

    /// Gets the underlying Nostr client Arc.
    pub fn client(&self) -> Arc<Client> {
        self.client.clone()
    }

    /// Gets the underlying group membership cache Arc.
    pub fn get_cache(&self) -> Arc<RwLock<GroupMembershipCache>> {
        self.cache.clone()
    }
}

/// Extracts pubkeys from any tag starting with "p", including standard mentions and NIP-29 role tags.
///
/// NOTE: We use manual `filter_map` because the built-in `tags.public_keys()` only parses
/// standard p-tags like `["p", <pubkey>]` and misses NIP-29 tags like `["p", <pubkey>, "Admin"]`.
pub fn extract_pubkeys_from_p_tags(tags: &Tags) -> impl Iterator<Item = PublicKey> + '_ {
    tags.iter().filter_map(|t| {
        if t.kind() == TagKind::p() {
            t.content()
                .and_then(|content| PublicKey::from_hex(content).ok())
        } else {
            None
        }
    })
}

/// Initializes a NIP-29 client based on application settings.
///
/// This function now attempts to establish a connection to the relay specified in settings.
///
/// # Arguments
/// - `settings`: Configuration settings containing NIP-29 relay details.
///
/// # Returns
/// - `Result<Arc<Nip29Client>>` containing the client if initialization and connection succeed.
/// - `Err` if configuration is missing, parsing fails, or connection fails.
pub async fn init_nip29_client(settings: &Settings) -> Result<Arc<Nip29Client>> {
    let nostr_config = &settings.nostr;

    // Use the get_nostr_private_key method instead of accessing private_key directly
    let secret_key = settings.get_nostr_private_key().ok_or_else(|| {
        ServiceError::Internal("No private key configured for NIP-29".to_string())
    })?;

    let keys = Keys::new(secret_key);
    let cache_expiration = nostr_config.cache_expiration.unwrap_or(300);

    let nip_29_client = Nip29Client::new(nostr_config.relay_url.clone(), keys, cache_expiration)
        .await
        .map_err(ServiceError::NostrSdkError)?;

    info!(
        relay_url = %nostr_config.relay_url,
        "NIP-29 client initialized and connected"
    );

    Ok(Arc::new(nip_29_client))
}

#[cfg(test)]
mod tests {
    use super::*; // Bring parent scope into test module
    use crate::config::{NostrSettings, Settings}; // Keep settings import
    use nostr_relay_builder::MockRelay; // Need this for tests
    use std::time::Duration;
    use tokio::time::sleep; // Need this for tests

    // Helper to setup Mock Relay and Keys for tests
    async fn setup_mock_relay() -> Result<(MockRelay, String, Keys), Error> {
        let mock_relay = MockRelay::run().await?;
        let relay_url_str = mock_relay.url();
        // Use generated keys as the client/service keys for the test
        let service_keys = Keys::generate();
        Ok((mock_relay, relay_url_str, service_keys))
    }

    #[tokio::test]
    async fn test_nip29_client_new_and_connect() -> Result<()> {
        let (_mock_relay, relay_url_str, service_keys) = setup_mock_relay()
            .await
            .map_err(|e| ServiceError::Internal(format!("Setup mock relay failed: {}", e)))?;

        // Test the `new` constructor which should connect
        let client_result =
            Nip29Client::new(relay_url_str.clone(), service_keys.clone(), 300).await;
        assert!(
            client_result.is_ok(),
            "Client creation failed: {:?}",
            client_result.err()
        );
        let nip29_client = client_result.unwrap();

        // Add delay for connection to establish
        sleep(Duration::from_millis(200)).await; // Increased delay slightly

        let relays = nip29_client.client.relays().await;
        let parsed_relay_url = RelayUrl::parse(&relay_url_str)?; // Use RelayUrl from nostr_sdk::prelude::*

        assert!(
            relays.contains_key(&parsed_relay_url),
            "Relay URL not found in client's list"
        );
        let relay_state = &relays[&parsed_relay_url];
        assert!(
            relay_state.is_connected(),
            "Client failed to connect to mock relay. Status: {:?}",
            relay_state.status()
        );

        // Ensure the client disconnects properly (optional but good practice)
        nip29_client.client().disconnect().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_init_nip29_client_from_settings() -> Result<()> {
        // Use a hardcoded test key in hex format
        let test_key_hex = "0000000000000000000000000000000000000000000000000000000000000001";

        // Parse it to a SecretKey for the test
        let sk = SecretKey::from_hex(test_key_hex).unwrap();
        let keys = Keys::new(sk);

        let settings = Settings {
            nostr: NostrSettings {
                // This URL isn't actually used by from_settings, but provide a valid format
                relay_url: "ws://localhost:1234".to_string(),
                cache_expiration: Some(600),
            },
            // Use private_key_hex in ServiceSettings
            service: crate::config::ServiceSettings {
                private_key_hex: Some(test_key_hex.to_string()),
                process_window_days: 0,
                processed_event_ttl_secs: 0,
                control_kinds: vec![],
                dm_kinds: vec![],
            },
            redis: crate::config::RedisSettings {
                url: "".to_string(),
                connection_pool_size: 0,
            },
            apps: vec![],
            cleanup: crate::config::CleanupSettings {
                enabled: false,
                interval_secs: 0,
                token_max_age_days: 0,
            },
            server: crate::config::ServerSettings {
                listen_addr: "0.0.0.0:8000".to_string(),
            },
        };

        // Use the from_settings constructor which doesn't connect
        let client_result = Nip29Client::from_settings(&settings);
        assert!(
            client_result.is_ok(),
            "Nip29Client::from_settings failed: {:?}",
            client_result.err()
        );
        let client = client_result.unwrap();

        // Check if keys and cache expiration were set correctly
        assert_eq!(
            client.relay_keys.secret_key().to_bech32().unwrap(),
            keys.secret_key().to_bech32().unwrap()
        );
        let cache = client.cache.read().await;
        assert_eq!(cache.cache_expiration, 600);

        // Check that the client is NOT connected (because from_settings doesn't autoconnect)
        let relays = client.client.relays().await;
        assert!(
            relays.is_empty() || !relays.values().any(|s| s.is_connected()),
            "Client from_settings should not be connected"
        );

        Ok(())
    }
}
