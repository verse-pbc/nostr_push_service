#!/bin/bash
# Load Firebase config
source .env.local

# Generate a test private key for NIP-29 if not set
if [ -z "$NOSTR_PUSH__SERVICE__PRIVATE_KEY_HEX" ]; then
    export NOSTR_PUSH__SERVICE__PRIVATE_KEY_HEX="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
fi

# Set all required environment variables
export GOOGLE_CLOUD_PROJECT="${FIREBASE_PROJECT_ID}"
export GOOGLE_APPLICATION_CREDENTIALS="./firebase-service-account.json"
export REDIS_URL="redis://localhost:6379"
export RUST_LOG="${RUST_LOG:-info}"

echo "Starting server with:"
echo "  GOOGLE_CLOUD_PROJECT=$GOOGLE_CLOUD_PROJECT"
echo "  GOOGLE_APPLICATION_CREDENTIALS=$GOOGLE_APPLICATION_CREDENTIALS"
echo ""

cargo run
