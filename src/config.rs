use serde::Deserialize;

// Re-export config crate error if needed, or use custom error
pub use config::ConfigError;

#[derive(Debug, Deserialize, Clone)]
pub struct Settings {
    pub nostr: NostrSettings,
    pub service: ServiceSettings,
    pub redis: RedisSettings,
    pub apps: Vec<AppConfig>,
    pub cleanup: CleanupSettings,
    #[serde(default = "default_server_settings")]
    pub server: ServerSettings,
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
    pub control_kinds: Vec<u64>,  // Push control events (3079-3082)
    pub dm_kinds: Vec<u64>,        // DM kinds to monitor globally (1059, 14)
}

#[derive(Debug, Deserialize, Clone)]
pub struct RedisSettings {
    pub url: String, // Loaded via env var typically
    pub connection_pool_size: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FrontendConfig {
    pub api_key: String,
    pub auth_domain: String,
    pub project_id: String,
    pub storage_bucket: String,
    pub messaging_sender_id: String,
    pub app_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurement_id: Option<String>,
    pub vapid_public_key: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub name: String,
    pub frontend_config: FrontendConfig,
    pub credentials_path: String,  // Required - must be specified in config
    #[serde(default)]
    pub allowed_subscription_kinds: Vec<u64>,  // Whitelist for user subscriptions per app
}

// Keep FcmSettings for backward compatibility with FcmClient
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

#[derive(Debug, Deserialize, Clone)]
pub struct ServerSettings {
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
}

fn default_server_settings() -> ServerSettings {
    ServerSettings {
        listen_addr: default_listen_addr(),
    }
}

fn default_listen_addr() -> String {
    "0.0.0.0:8000".to_string()
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        let config_dir = std::env::current_dir().expect("Failed to get current dir");
        
        // Default to development environment for safety
        let env = std::env::var("APP_ENV").unwrap_or_else(|_| "development".to_string());
        
        // Select config file based on environment
        let config_filename = match env.as_str() {
            "production" => "settings.yaml",
            "development" => "settings.development.yaml",
            other => {
                // For any other environment, try settings.{env}.yaml
                // This allows for staging, test, etc.
                &format!("settings.{}.yaml", other)
            }
        };
        
        let config_path = config_dir.join("config").join(config_filename);
        

        let s = config::Config::builder()
            .add_source(config::File::from(config_path).required(true))
            // Eg.. `NOSTR_PUSH__REDIS__URL=redis://...` would override `redis.url`
            .add_source(config::Environment::with_prefix("NOSTR_PUSH").separator("__"))
            .build()?;

        s.try_deserialize()
    }

    // Helper method to get service keys (for relay auth and NIP-44 encryption)
    pub fn get_service_keys(&self) -> Option<nostr_sdk::Keys> {
        // It will be overridden by NOSTR_PUSH__SERVICE__PRIVATE_KEY_HEX if set.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_loads_with_new_fields() {
        // This test verifies that the config loads with the new frontend_config structure
        let result = Settings::new();
        
        match result {
            Ok(settings) => {
                // Verify control_kinds contains the push control events
                assert_eq!(settings.service.control_kinds, vec![3079, 3080, 3081, 3082]);
                
                // Verify dm_kinds contains the encrypted DM kinds
                assert_eq!(settings.service.dm_kinds, vec![1059, 4]);
                
                // Verify frontend_config for nostrpushdemo app
                let nostrpushdemo = settings.apps.iter().find(|app| app.name == "nostrpushdemo").unwrap();
                assert_eq!(nostrpushdemo.frontend_config.project_id, "plur-push-local");
                assert_eq!(nostrpushdemo.frontend_config.api_key, "AIzaSyBVZr13kC2niDhmJX2E0oMhGRlDqmC1wSA");
                assert_eq!(nostrpushdemo.frontend_config.vapid_public_key, "BPVz4oPf--UKQHFkooAq1SROioo2AmaQ_e98wwjTtsHnfhB_wV2VL1cLzgTZKl3p9c-Ueev_0BNeMIvoCUz2LrM");
                assert_eq!(nostrpushdemo.allowed_subscription_kinds, vec![4, 9, 1059]);
                
                // Verify frontend_config for universes app
                let universes = settings.apps.iter().find(|app| app.name == "universes").unwrap();
                assert_eq!(universes.frontend_config.project_id, "universes-2bc44");
                assert_eq!(universes.frontend_config.api_key, "AIzaSyACtVJJqIGM-lIsALzXggdGJRpxl31U0iQ");
                assert_eq!(universes.frontend_config.vapid_public_key, "BNPu0TK2qLDu-TiW09NqNU8QGeQuLSyZ87fPJm9wMwYsIA2bt6Bzi5IfmA0NmhQNa5jaSnH_tsmLEw6t5OMR4H8");
                assert_eq!(universes.allowed_subscription_kinds, vec![3, 4, 6, 7, 1059, 1111, 4552, 30023, 34550, 34551, 34552, 34553, 9735]);
                
            }
            Err(e) => {
                panic!("Failed to load config: {}", e);
            }
        }
    }
}