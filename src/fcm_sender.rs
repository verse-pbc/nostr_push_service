use crate::{config::FcmSettings, error::Result, models::FcmPayload};
use async_trait::async_trait;
use firebase_messaging_rs::{
    fcm::{FCMApi, FCMError as FirebaseFCMError, Message, Notification},
    FCMClient as FirebaseClient,
};
// Removed unused import
// use serde_json;
use base64;
use std::sync::{Arc, Mutex};
use std::{collections::HashMap, time::Duration};
use thiserror::Error;
use tracing;

#[derive(Error, Debug, Clone)]
pub enum FcmError {
    #[error("Initialization error: {0}")]
    Initialization(String),
    #[error("FCM internal request error: {0}")]
    InternalRequest(String),
    #[error("FCM internal response error: {0}")]
    InternalResponse(String),
    #[error("Unauthorized: {0}")]
    Unauthorized(String),
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
    #[error("FCM indicated token is not registered or invalid")]
    TokenNotRegistered,
    #[error("Retryable internal error. Retry after: {0:?}")]
    RetryableInternal(Duration),
    #[error("FCM internal error")]
    InternalError,
    #[error("Unknown FCM error: code={code}, hint={hint:?}")]
    Unknown { code: u16, hint: Option<String> },
}

impl From<FirebaseFCMError> for FcmError {
    fn from(err: FirebaseFCMError) -> Self {
        match err {
            FirebaseFCMError::InternalRequestError { reason } => FcmError::InternalRequest(reason),
            FirebaseFCMError::InternalResponseError { reason } => {
                FcmError::InternalResponse(reason)
            }
            FirebaseFCMError::Unauthorized(reason) => FcmError::Unauthorized(reason),
            FirebaseFCMError::InvalidRequestDescriptive { reason } => {
                if reason.contains("invalid registration token")
                    || reason.contains("BadDeviceToken")
                    || reason.to_lowercase().contains("unregistered")
                    || reason.to_lowercase().contains("not registered")
                {
                    FcmError::TokenNotRegistered
                } else {
                    FcmError::InvalidRequest(reason)
                }
            }
            FirebaseFCMError::InvalidRequest => {
                FcmError::InvalidRequest("Unknown invalid request".to_string())
            }
            FirebaseFCMError::RetryableInternal { retry_after } => {
                FcmError::RetryableInternal(retry_after)
            }
            FirebaseFCMError::Internal => FcmError::InternalError,
            FirebaseFCMError::Unknown { code, hint } => FcmError::Unknown { code, hint },
        }
    }
}

// Define the trait for sending FCM messages
#[async_trait]
pub trait FcmSend: Send + Sync {
    async fn send_single(
        &self,
        token: &str,
        payload: FcmPayload,
    ) -> std::result::Result<(), FcmError>;
}

// Implementation for the real Firebase client
struct RealFcmClient {
    client: FirebaseClient,
}

impl RealFcmClient {
    fn new() -> Result<Self, FcmError> {
        // Note: Project ID logic moved to AppState initialization or configuration loading
        // to avoid env var side effects here and ensure it's set before calling this.

        let client_result = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
            rt.block_on(async { FirebaseClient::new().await })
        })
        .join()
        .expect("Tokio runtime thread panicked");

        let client = client_result.map_err(|e| {
            FcmError::Initialization(format!("Failed to initialize FirebaseClient: {}", e))
        })?;

        Ok(RealFcmClient { client })
    }
}

#[async_trait]
impl FcmSend for RealFcmClient {
    /// Sends a notification payload to a single FCM token using the real Firebase client.
    async fn send_single(
        &self,
        token: &str,
        payload: FcmPayload,
    ) -> std::result::Result<(), FcmError> {
        // Support both notification+data and data-only messages
        let notification = payload.notification.map(|notif| Notification {
            title: notif.title,
            body: notif.body,
            image: None,
        });
        
        let message = Message::Token {
            token: token.to_string(),
            name: None,
            notification,  // Can be None for data-only messages
            data: payload.data,
            android: None,
            apns: None,
            webpush: None,
            fcm_options: None,
        };

        tracing::info!(
            "Sending simplified FCM request for token prefix {}...",
            &token[..std::cmp::min(token.len(), 8)]
        );

        match self.client.send(&message).await {
            Ok(_response) => {
                tracing::info!(
                    "FCM send successful for token prefix {}",
                    &token[..std::cmp::min(token.len(), 8)]
                );
                Ok(())
            }
            Err(firebase_err) => {
                let custom_error = FcmError::from(firebase_err);
                tracing::error!(
                    "FCM send failed for token prefix {}: {:?}",
                    &token[..std::cmp::min(token.len(), 8)],
                    custom_error
                );
                Err(custom_error)
            }
        }
    }
}

// The public FcmClient now holds a trait object
pub struct FcmClient {
    // Changed client type to a Boxed trait object
    client: Box<dyn FcmSend>,
}

impl FcmClient {
    // Updated new to initialize with the RealFcmClient implementation
    pub fn new(settings: &FcmSettings) -> Result<Self, FcmError> {
        // Handle credentials from base64 environment variable
        if let Ok(credentials_base64) = std::env::var("NOSTR_PUSH__FCM__CREDENTIALS_BASE64") {
            if !credentials_base64.is_empty() {
                // Decode base64 credentials
                use base64::Engine;
                match base64::engine::general_purpose::STANDARD.decode(&credentials_base64) {
                    Ok(credentials_json) => {
                        // Write to a temporary file
                        let temp_dir = std::env::temp_dir();
                        let creds_path = temp_dir.join("firebase-service-account.json");
                        
                        match std::fs::write(&creds_path, credentials_json) {
                            Ok(_) => {
                                tracing::info!("Wrote Firebase credentials to temporary file: {:?}", creds_path);
                                // Set GOOGLE_APPLICATION_CREDENTIALS to the temp file path
                                std::env::set_var("GOOGLE_APPLICATION_CREDENTIALS", creds_path.to_str().unwrap());
                                tracing::info!("Set GOOGLE_APPLICATION_CREDENTIALS environment variable");
                            },
                            Err(e) => {
                                tracing::error!("Failed to write Firebase credentials to temp file: {}", e);
                                return Err(FcmError::Initialization(format!("Failed to write credentials: {}", e)));
                            }
                        }
                    },
                    Err(e) => {
                        tracing::error!("Failed to decode base64 Firebase credentials: {}", e);
                        return Err(FcmError::Initialization(format!("Failed to decode credentials: {}", e)));
                    }
                }
            }
        }
        
        // Important: GOOGLE_CLOUD_PROJECT env var handling moved.
        // Ensure it's set appropriately *before* calling this function,
        // likely during application startup/config loading.
        if std::env::var("GOOGLE_CLOUD_PROJECT").is_err() {
            if !settings.project_id.is_empty() {
                tracing::debug!(
                    "Setting GOOGLE_CLOUD_PROJECT env var to: {}",
                    &settings.project_id
                );
                std::env::set_var("GOOGLE_CLOUD_PROJECT", &settings.project_id);
            } else {
                tracing::warn!(
                     "GOOGLE_CLOUD_PROJECT env var not set and FcmSettings.project_id is empty. FCM initialization might fail."
                 );
            }
        } else {
            tracing::debug!("GOOGLE_CLOUD_PROJECT env var already set.");
        }

        let real_client = RealFcmClient::new()?;
        Ok(FcmClient {
            client: Box::new(real_client),
        })
    }

    // Add a constructor for injecting a mock/custom implementation (for testing)
    pub fn new_with_impl(client_impl: Box<dyn FcmSend>) -> Self {
        FcmClient {
            client: client_impl,
        }
    }

    /// Sends a notification payload to a batch of tokens.
    /// Delegates to the underlying FcmSend implementation's send_single.
    pub async fn send_batch(
        &self,
        tokens: &[String],
        payload: FcmPayload,
    ) -> HashMap<String, std::result::Result<(), FcmError>> {
        let mut results = HashMap::new();
        for token in tokens {
            // Delegate to the trait object's method
            let result = self.client.send_single(token, payload.clone()).await;
            results.insert(token.clone(), result);
        }
        // Note: We are not returning a Result here anymore, but a HashMap of results.
        // The original code returned Result<HashMap<...>, FcmError> which seemed incorrect
        // as individual sends could fail without the whole batch failing.
        // If a top-level error IS needed (e.g., impossible to even start sending),
        // this signature might need adjustment, but usually, per-token results are desired.
        results // Return the map directly
    }

    /// Sends a notification payload to a single FCM token.
    /// Delegates directly to the underlying FcmSend implementation.
    pub async fn send_single(
        &self,
        token: &str,
        payload: FcmPayload,
    ) -> std::result::Result<(), FcmError> {
        // Delegate to the trait object's method
        self.client.send_single(token, payload).await
    }
}

// Define the mock FCM sender struct (Now public and outside cfg(test))
#[derive(Clone, Default)]
pub struct MockFcmSender {
    // Store sent messages: Arc<Mutex<Vec<(token, payload)>>>
    sent_messages: Arc<Mutex<Vec<(String, FcmPayload)>>>, // Made fields crate-public for direct access if needed, or keep private and use methods
    // Optional: Simulate specific errors for certain tokens
    error_tokens: Arc<Mutex<HashMap<String, FcmError>>>,
}

// Make methods public
impl MockFcmSender {
    pub fn new() -> Self {
        Self {
            sent_messages: Arc::new(Mutex::new(Vec::new())),
            error_tokens: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    // Helper to retrieve sent messages for assertions
    pub fn get_sent_messages(&self) -> Vec<(String, FcmPayload)> {
        self.sent_messages.lock().unwrap().clone()
    }

    // Helper to simulate errors for specific tokens
    pub fn set_error_for_token(&self, token: &str, error: FcmError) {
        self.error_tokens
            .lock()
            .unwrap()
            .insert(token.to_string(), error);
    }

    // Helper to clear recorded messages and errors (useful between tests)
    pub fn clear(&self) {
        self.sent_messages.lock().unwrap().clear();
        self.error_tokens.lock().unwrap().clear();
    }
}

#[async_trait]
impl FcmSend for MockFcmSender {
    async fn send_single(
        &self,
        token: &str,
        payload: FcmPayload,
    ) -> std::result::Result<(), FcmError> {
        // Check if this token should simulate an error
        if let Some(error) = self.error_tokens.lock().unwrap().get(token) {
            tracing::warn!(
                "MockFcmSender: Simulating error {:?} for token prefix {}",
                error,
                &token[..std::cmp::min(token.len(), 8)]
            );
            return Err(error.clone()); // Return the predefined error
        }

        // Otherwise, record the message
        tracing::info!(
            "MockFcmSender: Recording send for token prefix {}...",
            &token[..std::cmp::min(token.len(), 8)]
        );
        let mut messages = self.sent_messages.lock().unwrap();
        messages.push((token.to_string(), payload));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*; // Import items from parent module (fcm_sender)
    use crate::models::{FcmNotification, FcmPayload};
    
    
    use tokio; // Ensure tokio is available for async tests

    // Example test demonstrating how to use the mock
    #[tokio::test]
    async fn test_mock_fcm_sender_single_send() {
        // Ensure FcmPayload derives PartialEq and Clone for this assertion
        let mock_sender = MockFcmSender::new(); // Create instance directly
                                                // Pass a boxed clone to the client
        let fcm_client = FcmClient::new_with_impl(Box::new(mock_sender.clone()));

        let token = "test_token_1";
        let payload = FcmPayload {
            notification: Some(FcmNotification {
                title: Some("Test Title".to_string()),
                body: Some("Test Body".to_string()),
            }),
            data: None, // Add data if needed
            android: None,
            webpush: None,
            apns: None,
        };

        // Use the FcmClient (which internally uses the mock)
        let result = fcm_client.send_single(token, payload.clone()).await;

        assert!(result.is_ok());

        // Verify the message was recorded by the mock (use original instance)
        let sent = mock_sender.get_sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, token);
        assert_eq!(sent[0].1, payload);
    }

    #[tokio::test]
    async fn test_mock_fcm_sender_batch_send() {
        // Ensure FcmPayload derives PartialEq and Clone
        let mock_sender = MockFcmSender::new(); // Create instance directly
                                                // Pass a boxed clone to the client
        let fcm_client = FcmClient::new_with_impl(Box::new(mock_sender.clone()));

        let tokens = vec!["token1".to_string(), "token2".to_string()];
        let payload = FcmPayload {
            notification: Some(FcmNotification {
                title: Some("Batch Title".to_string()),
                body: Some("Batch Body".to_string()),
            }),
            data: None,
            android: None,
            webpush: None,
            apns: None,
        };

        // Simulate an error for one token (use original instance)
        mock_sender.set_error_for_token("token2", FcmError::TokenNotRegistered);

        // Use the FcmClient's batch send (which calls send_single repeatedly)
        let results = fcm_client.send_batch(&tokens, payload.clone()).await;

        // Verify results map
        assert_eq!(results.len(), 2);
        assert!(results.get("token1").unwrap().is_ok());
        assert!(matches!(
            results.get("token2").unwrap(),
            Err(FcmError::TokenNotRegistered)
        ));

        // Verify only the successful message was recorded by the mock (use original instance)
        let sent = mock_sender.get_sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "token1");
        assert_eq!(sent[0].1, payload);
    }

    #[tokio::test]
    async fn test_mock_fcm_sender_error_simulation() {
        // Ensure FcmPayload derives PartialEq and Clone
        let mock_sender = MockFcmSender::new(); // Create instance directly
                                                // Pass a boxed clone to the client
        let fcm_client = FcmClient::new_with_impl(Box::new(mock_sender.clone()));

        let error_token = "error_token";
        let simulated_error = FcmError::InternalError; // Example error
                                                       // Set error using original instance
        mock_sender.set_error_for_token(error_token, simulated_error.clone());

        let payload = FcmPayload {
            notification: Some(FcmNotification {
                title: Some("Error Test".to_string()),
                body: Some("Error Body".to_string()),
            }),
            data: None,
            android: None,
            webpush: None,
            apns: None,
        };

        // Send to the token that should produce an error
        let result = fcm_client.send_single(error_token, payload.clone()).await;

        // Assert that the expected error was returned
        assert!(result.is_err());
        // Use matches! macro for better error checking if FcmError doesn't impl PartialEq
        assert!(matches!(result.unwrap_err(), FcmError::InternalError));

        // Assert that no message was recorded for the error token (use original instance)
        let sent = mock_sender.get_sent_messages();
        assert!(sent.is_empty());
    }
}
