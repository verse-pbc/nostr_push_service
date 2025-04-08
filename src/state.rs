use crate::{
    config::Settings,
    error::Result,
    fcm_sender::FcmClient,
    redis_store::{self, RedisPool},
};
use nostr_sdk::Keys;
use std::{env, sync::Arc};

use crate::error::ServiceError;
use crate::nostr::nip29::{init_nip29_client, Nip29Client};

/// Shared application state
pub struct AppState {
    pub settings: Settings,
    pub redis_pool: RedisPool,
    pub fcm_client: Arc<FcmClient>,
    pub service_keys: Option<Keys>,
    pub nip29_client: Arc<Nip29Client>,
}

impl AppState {
    pub async fn new(settings: Settings) -> Result<Self> {
        // Determine Redis URL: prioritize REDIS_URL env var over settings
        let redis_url = match env::var("REDIS_URL") {
            Ok(url_from_env) => {
                tracing::info!(
                    "Using Redis URL from REDIS_URL environment variable: {}",
                    url_from_env
                );
                url_from_env
            }
            Err(_) => {
                tracing::info!(
                    "REDIS_URL environment variable not set. Using Redis URL from settings: {}",
                    &settings.redis.url
                );
                settings.redis.url.clone() // Clone the URL from settings
            }
        };

        let redis_pool =
            redis_store::create_pool(&redis_url, settings.redis.connection_pool_size).await?;
        let fcm_client = Arc::new(FcmClient::new(&settings.fcm)?);
        let service_keys = settings.get_service_keys();
        let nip29_client = init_nip29_client(&settings).await.map_err(|e| {
            ServiceError::Internal(format!("Failed to initialize Nip29Client: {}", e))
        })?;

        Ok(AppState {
            settings,
            redis_pool,
            fcm_client,
            service_keys,
            nip29_client,
        })
    }
}
