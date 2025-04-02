use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Structure for the FCM message payload (customize as needed)
// See: https://firebase.google.com/docs/reference/fcm/rest/v1/projects.messages#Message
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FcmPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notification: Option<FcmNotification>,
    pub data: Option<HashMap<String, String>>,

    // Platform specific overrides (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub android: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webpush: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apns: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct FcmNotification {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

// Represents information about a device token retrieved from Redis
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DeviceTokenInfo {
    pub token: String,
    // pub platform: String, // e.g., "ios", "android", "web"
}
