use serde::Deserialize;

// Re-export config crate error if needed, or use custom error
pub use config::ConfigError;

#[derive(Debug, Deserialize, Clone)]
pub struct Settings {
    pub nostr: NostrSettings,
    pub service: ServiceSettings,
    pub redis: RedisSettings,
    pub fcm: FcmSettings,
    pub cleanup: CleanupSettings,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NostrSettings {
    pub relay_url: String,
    pub cache_expiration: Option<u64>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServiceSettings {
    pub private_key_hex: Option<String>,
    pub process_window_days: i64,
    pub processed_event_ttl_secs: u64,
    pub listen_kinds: Vec<u64>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RedisSettings {
    pub url: String, // Loaded via env var typically
    pub connection_pool_size: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FcmSettings {
    pub project_id: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CleanupSettings {
    pub enabled: bool,
    pub interval_secs: u64,
    pub token_max_age_days: i64,
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        let config_dir = std::env::current_dir().expect("Failed to get current dir");
        let config_path = config_dir.join("config").join("settings.yaml");

        let s = config::Config::builder()
            .add_source(config::File::from(config_path).required(true))
            // Eg.. `PLUR_PUSH__REDIS__URL=redis://...` would override `redis.url`
            .add_source(config::Environment::with_prefix("PLUR_PUSH").separator("__"))
            .build()?;

        s.try_deserialize()
    }

    // Helper method to get service keys
    pub fn get_service_keys(&self) -> Option<nostr_sdk::Keys> {
        // It will be overridden by PLUR_PUSH__SERVICE__PRIVATE_KEY_HEX if set.
        let key_hex = self.service.private_key_hex.as_deref()?;
        let secret_key = nostr_sdk::SecretKey::from_hex(key_hex).ok()?;
        Some(nostr_sdk::Keys::new(secret_key))
    }

    // Helper method to get the private key for NIP-29
    pub fn get_nostr_private_key(&self) -> Option<nostr_sdk::SecretKey> {
        // Use the same private_key_hex from ServiceSettings
        let key_hex = self.service.private_key_hex.as_deref()?;
        nostr_sdk::SecretKey::from_hex(key_hex).ok()
    }
}
