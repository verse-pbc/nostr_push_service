use crate::{
    config::Settings,
    crypto::CryptoService,
    error::Result,
    fcm_sender::FcmClient,
    redis_store::{self, RedisPool},
};
use nostr_sdk::Keys;
use std::{collections::{HashMap, HashSet}, env, sync::Arc};

use crate::error::ServiceError;
use crate::nostr::nip29::{init_nip29_client, Nip29Client};

/// Shared application state
pub struct AppState {
    pub settings: Settings,
    pub redis_pool: RedisPool,
    pub fcm_clients: HashMap<String, Arc<FcmClient>>,
    pub supported_apps: HashSet<String>,
    pub service_keys: Option<Keys>,
    pub crypto_service: Option<CryptoService>,
    pub nip29_client: Arc<Nip29Client>,
}

impl AppState {
    pub async fn new(settings: Settings) -> Result<Self> {
        // Determine Redis URL: prioritize REDIS_URL env var over settings
        let redis_url = match env::var("REDIS_URL") {
            Ok(url_from_env) => {
                tracing::info!("Using Redis URL from REDIS_URL environment variable");
                url_from_env
            }
            Err(_) => {
                tracing::info!(
                    "REDIS_URL environment variable not set. Using Redis URL from settings"
                );
                settings.redis.url.clone()
            }
        };

        let redis_pool =
            redis_store::create_pool(&redis_url, settings.redis.connection_pool_size).await?;
        
        // Initialize FCM clients for each configured app
        // 
        // IMPORTANT: Credential Handling Workaround
        // ==========================================
        // The firebase-messaging-rs library uses gcloud-sdk which only supports reading credentials
        // from environment variables (GOOGLE_APPLICATION_CREDENTIALS) via TokenSourceType::Default.
        // 
        // Since we need different service accounts for different Firebase projects (each project
        // requires its own service account with appropriate permissions), we implement a sequential
        // initialization workaround:
        // 
        // 1. Set GOOGLE_APPLICATION_CREDENTIALS for app N
        // 2. Create FCM client for app N (reads credentials at creation time)
        // 3. Clear/reset env var for next app
        // 
        // This is safe because:
        // - It happens once at startup before any concurrent operations
        // - Each FCM client captures its credentials during initialization
        // - Once created, clients don't re-read the env var
        // 
        // TODO: This could be improved if firebase-messaging-rs exposed TokenSourceType::File
        // or TokenSourceType::Json to allow passing credentials directly without env var manipulation.
        // See: https://github.com/i10416/firebase-messaging-rs/issues/[TODO]
        
        let mut fcm_clients = HashMap::new();
        let mut supported_apps = HashSet::new();
        
        for (index, app_config) in settings.apps.iter().enumerate() {
            // Try to set up credentials for this app
            let credentials_set = 
                // First try path-based credentials (for K8s deployments)
                if let Ok(credentials_path) = env::var(&format!("FCM_APPS__{}__CREDENTIALS_PATH", index)) {
                    if !credentials_path.is_empty() {
                        // Set global env var that gcloud-sdk will read during FCM client initialization
                        env::set_var("GOOGLE_APPLICATION_CREDENTIALS", &credentials_path);
                        tracing::info!("Set credentials path for app {}: {}", app_config.name, credentials_path);
                        true
                    } else {
                        false
                    }
                // Then try base64 credentials (for testing/local dev)
                } else if let Ok(credentials_base64) = env::var(&format!("FCM_APPS__{}__CREDENTIALS_BASE64", index)) {
                    if !credentials_base64.is_empty() {
                        env::set_var("NOSTR_PUSH__FCM__CREDENTIALS_BASE64", credentials_base64);
                        tracing::info!("Set base64 credentials for app {}", app_config.name);
                        true
                    } else {
                        false
                    }
                // Fallback: try global credentials if this is the first app
                } else if index == 0 {
                    // Check if global credentials are already set
                    if env::var("GOOGLE_APPLICATION_CREDENTIALS").is_ok() || 
                       env::var("NOSTR_PUSH__FCM__CREDENTIALS_BASE64").is_ok() {
                        tracing::info!("Using global credentials for app {}", app_config.name);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
            
            if credentials_set {
                // Create FCM client for this app with its specific project_id
                let fcm_settings = crate::config::FcmSettings {
                    project_id: app_config.fcm_project_id.clone(),
                };
                
                match FcmClient::new(&fcm_settings) {
                    Ok(client) => {
                        fcm_clients.insert(app_config.name.clone(), Arc::new(client));
                        supported_apps.insert(app_config.name.clone());
                        tracing::info!("Initialized FCM client for app '{}' with project '{}'", 
                                     app_config.name, app_config.fcm_project_id);
                    }
                    Err(e) => {
                        tracing::error!("Failed to initialize FCM client for app {}: {}", app_config.name, e);
                    }
                }
                
                // Clear app-specific env vars after initialization to prepare for next app
                // This ensures the next app doesn't accidentally use the wrong credentials
                // We only clear NOSTR_PUSH__FCM__CREDENTIALS_BASE64 since GOOGLE_APPLICATION_CREDENTIALS
                // will be overwritten by the next app anyway
                if index > 0 || env::var(&format!("FCM_APPS__{}__CREDENTIALS_PATH", index)).is_ok() ||
                   env::var(&format!("FCM_APPS__{}__CREDENTIALS_BASE64", index)).is_ok() {
                    env::remove_var("NOSTR_PUSH__FCM__CREDENTIALS_BASE64");
                }
            } else {
                tracing::warn!("No FCM credentials found for app {} (index {})", app_config.name, index);
            }
        }
        
        if fcm_clients.is_empty() {
            tracing::error!("No FCM clients initialized - push notifications will not work");
        } else {
            tracing::info!("Initialized {} FCM client(s) for {} app(s)", 
                         fcm_clients.len(), supported_apps.len());
        }
        let service_keys = settings.get_service_keys();
        
        // Create crypto service if we have service keys
        let crypto_service = service_keys.as_ref().map(|keys| {
            CryptoService::new(keys.clone())
        });
        
        let nip29_client = init_nip29_client(&settings).await.map_err(|e| {
            ServiceError::Internal(format!("Failed to initialize Nip29Client: {}", e))
        })?;

        if fcm_clients.is_empty() {
            tracing::warn!("No FCM clients initialized. Push notifications will not work.");
        }
        
        Ok(AppState {
            settings,
            redis_pool,
            fcm_clients,
            supported_apps,
            service_keys,
            crypto_service,
            nip29_client,
        })
    }
}
