use blake3;
use nostr_sdk::prelude::*;
use nostr_sdk::nips::nip44;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use tracing::{debug, error, warn};

#[derive(Error, Debug)]
pub enum CryptoError {
    #[error("NIP-44 decryption failed: {0}")]
    DecryptionFailed(String),
    
    #[error("Invalid JSON payload: {0}")]
    InvalidJson(#[from] serde_json::Error),
    
    #[error("Missing service keypair")]
    MissingKeypair,
    
    #[error("Invalid public key: {0}")]
    InvalidPublicKey(String),
    
    #[error("Content is not properly encrypted (NIP-44 required)")]
    NotEncrypted,
    
    #[error("Invalid token: {0}")]
    InvalidToken(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenPayload {
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FilterUpsertPayload {
    pub filter: serde_json::Value, // The actual Nostr filter object
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FilterDeletePayload {
    pub filter: serde_json::Value, // The filter to remove
}

/// Generate a deterministic hash from a Nostr filter for storage/lookup
pub fn hash_filter(filter: &serde_json::Value) -> String {
    // Normalize the filter by sorting keys recursively
    let normalized = normalize_json_value(filter);
    
    // Serialize to a deterministic JSON string
    let json_str = serde_json::to_string(&normalized)
        .unwrap_or_else(|_| "{}".to_string());
    
    // Generate Blake3 hash (much faster than SHA-256)
    let hash = blake3::hash(json_str.as_bytes());
    
    // Return as hex string
    hash.to_hex().to_string()
}

/// Recursively normalize JSON values by sorting object keys
fn normalize_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut sorted_map = BTreeMap::new();
            for (k, v) in map {
                sorted_map.insert(k.clone(), normalize_json_value(v));
            }
            serde_json::Value::Object(sorted_map.into_iter().collect())
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(
                arr.iter().map(normalize_json_value).collect()
            )
        }
        _ => value.clone(),
    }
}

/// Service for handling NIP-44 encryption/decryption
pub struct CryptoService {
    keypair: Keys,
}

impl CryptoService {
    pub fn new(keypair: Keys) -> Self {
        Self { keypair }
    }
    
    pub fn from_secret_key(secret_key: SecretKey) -> Self {
        let keypair = Keys::new(secret_key);
        Self { keypair }
    }
    
    pub fn public_key(&self) -> PublicKey {
        self.keypair.public_key()
    }
    
    /// Decrypt NIP-44 encrypted content from a user
    pub fn decrypt_nip44(&self, encrypted_content: &str, sender_pubkey: &PublicKey) -> Result<String, CryptoError> {
        debug!("Decrypting NIP-44 content from pubkey: {}", sender_pubkey);
        
        // Decrypt using NIP-44 (standard format - base64 encoded payload directly)
        match nip44::decrypt(self.keypair.secret_key(), sender_pubkey, encrypted_content) {
            Ok(decrypted) => {
                debug!("Successfully decrypted NIP-44 content");
                Ok(decrypted)
            }
            Err(e) => {
                error!("Failed to decrypt NIP-44 content: {:?}", e);
                Err(CryptoError::DecryptionFailed(e.to_string()))
            }
        }
    }
    
    /// Decrypt and parse token payload from NIP-44 encrypted content
    pub fn decrypt_token_payload(&self, encrypted_content: &str, sender_pubkey: &PublicKey) -> Result<TokenPayload, CryptoError> {
        let decrypted = self.decrypt_nip44(encrypted_content, sender_pubkey)?;
        
        // Parse JSON payload
        let payload: TokenPayload = serde_json::from_str(&decrypted)?;
        
        // Validate token is not empty
        if payload.token.is_empty() {
            return Err(CryptoError::InvalidToken("Token cannot be empty".to_string()));
        }
        
        Ok(payload)
    }
    
    /// Decrypt and parse filter upsert payload from NIP-44 encrypted content
    pub fn decrypt_filter_upsert_payload(&self, encrypted_content: &str, sender_pubkey: &PublicKey) -> Result<FilterUpsertPayload, CryptoError> {
        let decrypted = self.decrypt_nip44(encrypted_content, sender_pubkey)?;
        
        // Parse JSON payload
        let payload: FilterUpsertPayload = serde_json::from_str(&decrypted)?;
        
        // Validate that filter is not empty
        if payload.filter.is_null() || payload.filter.as_object().map_or(true, |obj| obj.is_empty()) {
            return Err(CryptoError::InvalidToken("Filter cannot be empty".to_string()));
        }
        
        Ok(payload)
    }
    
    /// Decrypt and parse filter delete payload from NIP-44 encrypted content
    pub fn decrypt_filter_delete_payload(&self, encrypted_content: &str, sender_pubkey: &PublicKey) -> Result<FilterDeletePayload, CryptoError> {
        let decrypted = self.decrypt_nip44(encrypted_content, sender_pubkey)?;
        
        // Parse JSON payload
        let payload: FilterDeletePayload = serde_json::from_str(&decrypted)?;
        
        // Validate that filter is not empty
        if payload.filter.is_null() || payload.filter.as_object().map_or(true, |obj| obj.is_empty()) {
            return Err(CryptoError::InvalidToken("Filter cannot be empty".to_string()));
        }
        
        Ok(payload)
    }
    
    /// Check if content appears to be encrypted with NIP-44
    pub fn is_nip44_encrypted(content: &str) -> bool {
        // NIP-44 payloads are base64 encoded with version + nonce + ciphertext + mac
        // Minimum reasonable length for a NIP-44 encrypted payload
        content.len() > 100 && 
        content.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
    }
    
    /// Validate that content is properly encrypted (reject plaintext)
    pub fn validate_encrypted_content(content: &str) -> Result<(), CryptoError> {
        if !Self::is_nip44_encrypted(content) {
            warn!("Received plaintext or invalid token, rejecting");
            return Err(CryptoError::NotEncrypted);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_is_nip44_encrypted() {
        // Valid base64 strings of sufficient length
        assert!(CryptoService::is_nip44_encrypted("VGhpcyBpcyBhIHZlcnkgbG9uZyBiYXNlNjQgZW5jb2RlZCBzdHJpbmcgdGhhdCByZXByZXNlbnRzIGVuY3J5cHRlZCBkYXRhIGFuZCBzaG91bGQgcGFzcyB0aGUgdmFsaWRhdGlvbg=="));
        // Too short
        assert!(!CryptoService::is_nip44_encrypted("short"));
        // Contains invalid characters for base64
        assert!(!CryptoService::is_nip44_encrypted("plaintext!@#$%^&*()"));
        assert!(!CryptoService::is_nip44_encrypted("{\"token\":\"abc\"}"));
    }
    
    #[test]
    fn test_validate_encrypted_content() {
        // Valid base64 string of sufficient length
        assert!(CryptoService::validate_encrypted_content("VGhpcyBpcyBhIHZlcnkgbG9uZyBiYXNlNjQgZW5jb2RlZCBzdHJpbmcgdGhhdCByZXByZXNlbnRzIGVuY3J5cHRlZCBkYXRhIGFuZCBzaG91bGQgcGFzcyB0aGUgdmFsaWRhdGlvbg==").is_ok());
        // Invalid - too short
        assert!(CryptoService::validate_encrypted_content("short").is_err());
        // Invalid - contains non-base64 characters
        assert!(CryptoService::validate_encrypted_content("plaintext!@#").is_err());
        assert!(CryptoService::validate_encrypted_content("{\"token\":\"abc\"}").is_err());
    }
}