use crate::{
    config::Settings,
    crypto::CryptoService,
    error::Result,
    fcm_sender::FcmClient,
    handlers::CommunityHandler,
    redis_store::{self, RedisPool},
    subscriptions::SubscriptionManager,
};
use nostr_sdk::{Client, Keys, SubscriptionId};
use std::{collections::{HashMap, HashSet}, env, sync::Arc};
use tokio::sync::RwLock;
use tracing::{info, warn};

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
    pub nostr_client: Arc<Client>,  // Shared nostr client for direct subscription management
    pub user_subscriptions: Arc<RwLock<HashMap<String, SubscriptionId>>>,  // filter_hash -> SubscriptionId
    pub subscription_manager: Arc<SubscriptionManager>,
    pub community_handler: Arc<CommunityHandler>,
    pub notification_config: Option<crate::config::NotificationSettings>,
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
        
        for app_config in settings.apps.iter() {
            // Use the credentials path from configuration (required field)
            let credentials_path = &app_config.credentials_path;
            
            // Set GOOGLE_APPLICATION_CREDENTIALS for the library to use
            env::set_var("GOOGLE_APPLICATION_CREDENTIALS", credentials_path);
            tracing::info!("Setting credentials path for app '{}': {}", app_config.name, credentials_path);
            
            // Create FCM client for this app with its specific project_id
            let fcm_settings = crate::config::FcmSettings {
                project_id: app_config.frontend_config.project_id.clone(),
            };
            
            match FcmClient::new(&fcm_settings) {
                Ok(client) => {
                    fcm_clients.insert(app_config.name.clone(), Arc::new(client));
                    supported_apps.insert(app_config.name.clone());
                    tracing::info!("Initialized FCM client for app '{}' with project '{}'", 
                                 app_config.name, app_config.frontend_config.project_id);
                }
                Err(e) => {
                    tracing::error!("Failed to initialize FCM client for app {} (credentials: {}): {}", 
                                  app_config.name, credentials_path, e);
                }
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
        
        // Initialize subscription manager and community handler
        let subscription_manager = Arc::new(SubscriptionManager::new());
        let community_handler = Arc::new(CommunityHandler::new());
        
        // Get the nostr client from nip29_client
        let nostr_client = nip29_client.client();
        
        // Initialize the shared user subscriptions map
        let user_subscriptions = Arc::new(RwLock::new(HashMap::new()));

        // Clone notification config for use in event handler
        let notification_config = settings.notification.clone();

        // DEBUG: Log whether notification config loaded successfully
        if let Some(ref config) = notification_config {
            info!(
                "✅ Notification config loaded - profile_relays: {:?}, profile_cache_ttl: {}s, group_cache_ttl: {}s",
                config.profile_relays,
                config.profile_cache_ttl_secs,
                config.group_meta_cache_ttl_secs
            );
        } else {
            warn!("⚠️  Notification config is None - rich notifications DISABLED. Check settings.yaml 'notification:' section");
        }

        Ok(AppState {
            settings,
            redis_pool,
            fcm_clients,
            supported_apps,
            service_keys,
            crypto_service,
            nip29_client,
            nostr_client,
            user_subscriptions,
            subscription_manager,
            community_handler,
            notification_config,
        })
    }
}
