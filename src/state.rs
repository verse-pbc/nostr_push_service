use crate::{
    config::Settings,
    error::Result,
    fcm_sender::FcmClient,
    redis_store::{self, RedisPool},
};
use nostr_sdk::Keys;
use std::{env, sync::Arc};

/// Shared application state
pub struct AppState {
    pub settings: Settings,
    pub redis_pool: RedisPool,
    pub fcm_client: Arc<FcmClient>,
    pub service_keys: Option<Keys>,
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

        Ok(AppState {
            settings,
            redis_pool,
            fcm_client,
            service_keys,
        })
    }
}
