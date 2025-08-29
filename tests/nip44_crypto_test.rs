/// Integration tests for NIP-44 encryption/decryption functionality
use nostr_push_service::crypto::CryptoService;
use nostr_sdk::prelude::*;
use serde_json::json;

/// Test that NIP-44 encryption and decryption works correctly
#[test]
fn test_nip44_encryption_decryption() {
    // Generate keypairs
    let service_keys = Keys::generate();
    let user_keys = Keys::generate();
    
    // Create crypto service
    let crypto_service = CryptoService::new(service_keys.clone());
    
    // Create token payload
    let token = "test_fcm_token_12345";
    let payload = json!({ "token": token }).to_string();
    
    // Encrypt with user's private key to service's public key
    let encrypted = nip44::encrypt(
        user_keys.secret_key(),
        &service_keys.public_key(),
        payload.clone(),
        nip44::Version::V2,
    ).expect("Failed to encrypt");
    
    // Test with standard NIP-44 format (no prefix)
    let decrypted = crypto_service
        .decrypt_token_payload(&encrypted, &user_keys.public_key())
        .expect("Failed to decrypt");
    assert_eq!(decrypted.token, token);
}

/// Test that plaintext tokens are rejected
#[test]
fn test_plaintext_token_rejected() {
    let service_keys = Keys::generate();
    let crypto_service = CryptoService::new(service_keys);
    let user_pubkey = Keys::generate().public_key();
    
    // Test with plaintext token
    let plaintext = "plaintext_token";
    let result = crypto_service.decrypt_token_payload(plaintext, &user_pubkey);
    
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    // Plaintext will fail base64 decoding or decryption
    assert!(err_msg.contains("Failed to decrypt") || err_msg.contains("decoding") || err_msg.contains("not encrypted"), 
            "Unexpected error message: {}", err_msg);
}

/// Test that short/invalid base64 without prefix is rejected
#[test]
fn test_invalid_base64_rejected() {
    let service_keys = Keys::generate();
    let crypto_service = CryptoService::new(service_keys);
    let user_pubkey = Keys::generate().public_key();
    
    // Test with too short data (NIP-44 needs at least 100 chars)
    let short_data = "shortdata";
    let result = crypto_service.decrypt_token_payload(short_data, &user_pubkey);
    
    assert!(result.is_err());
    // It should fail validation or decryption
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("Failed to decrypt") || err_msg.contains("decoding") || err_msg.contains("not encrypted"), 
            "Unexpected error message: {}", err_msg);
}

/// Test that empty tokens are rejected
#[test]
fn test_empty_token_rejected() {
    let service_keys = Keys::generate();
    let user_keys = Keys::generate();
    let crypto_service = CryptoService::new(service_keys.clone());
    
    // Create payload with empty token
    let payload = json!({ "token": "" }).to_string();
    
    // Encrypt properly
    let encrypted = nip44::encrypt(
        user_keys.secret_key(),
        &service_keys.public_key(),
        payload,
        nip44::Version::V2,
    ).expect("Failed to encrypt");
    
    // Use standard format (no prefix)
    let encrypted_content = encrypted.clone();
    
    // Should fail due to empty token
    let result = crypto_service.decrypt_token_payload(&encrypted_content, &user_keys.public_key());
    
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Token cannot be empty"));
}

/// Test validation of encrypted content format
#[test]
fn test_validate_encrypted_content() {
    // Valid NIP-44 encrypted content (base64 string of sufficient length)
    let valid_base64 = "VGhpcyBpcyBhIHZlcnkgbG9uZyBiYXNlNjQgZW5jb2RlZCBzdHJpbmcgdGhhdCByZXByZXNlbnRzIGVuY3J5cHRlZCBkYXRhIGFuZCBzaG91bGQgcGFzcyB0aGUgdmFsaWRhdGlvbg==";
    assert!(CryptoService::validate_encrypted_content(valid_base64).is_ok());
    
    // Invalid - plaintext
    assert!(CryptoService::validate_encrypted_content("plaintext").is_err());
    
    // Invalid - too short
    assert!(CryptoService::validate_encrypted_content("short").is_err());
    
    // Invalid - empty
    assert!(CryptoService::validate_encrypted_content("").is_err());
    
    // Invalid - contains non-base64 characters
    assert!(CryptoService::validate_encrypted_content("invalid!@#$%").is_err());
}

/// Test that is_nip44_encrypted works correctly  
#[test]
fn test_is_nip44_encrypted() {
    // Valid base64 string of sufficient length
    let valid_base64 = "VGhpcyBpcyBhIHZlcnkgbG9uZyBiYXNlNjQgZW5jb2RlZCBzdHJpbmcgdGhhdCByZXByZXNlbnRzIGVuY3J5cHRlZCBkYXRhIGFuZCBzaG91bGQgcGFzcyB0aGUgdmFsaWRhdGlvbg==";
    assert!(CryptoService::is_nip44_encrypted(valid_base64));
    
    // Invalid - plaintext
    assert!(!CryptoService::is_nip44_encrypted("plaintext"));
    
    // Invalid - too short
    assert!(!CryptoService::is_nip44_encrypted("short"));
    
    // Invalid - empty
    assert!(!CryptoService::is_nip44_encrypted(""));
}