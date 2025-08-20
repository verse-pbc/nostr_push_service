#!/bin/bash

# Load Firebase credentials from .env.local
if [ -f .env.local ]; then
    export $(grep -v '^#' .env.local | xargs)
    echo "✅ Loaded Firebase configuration from .env.local"
else
    echo "❌ Error: .env.local not found!"
    echo "Run ./setup_firebase.sh first to create the template"
    exit 1
fi

# Check if service account file exists
if [ ! -f "${FIREBASE_SERVICE_ACCOUNT_PATH:-firebase-service-account.json}" ]; then
    echo "❌ Error: Firebase service account JSON not found!"
    echo "Download it from Firebase Console → Project Settings → Service Accounts"
    exit 1
fi

# Export Firebase config for dynamic serving
export FIREBASE_API_KEY
export FIREBASE_AUTH_DOMAIN  
export FIREBASE_PROJECT_ID
export FIREBASE_STORAGE_BUCKET
export FIREBASE_MESSAGING_SENDER_ID
export FIREBASE_APP_ID

echo "✅ Firebase configuration loaded and exported"

# Convert service account JSON to base64
CREDENTIALS_BASE64=$(base64 < "${FIREBASE_SERVICE_ACCOUNT_PATH}")

# Start Redis if not running
if ! pgrep redis-server > /dev/null; then
    redis-server --daemonize yes
    echo "✅ Started Redis server"
else
    echo "✅ Redis is already running"
fi

# Generate a test service private key if not provided
if [ -z "$SERVICE_PRIVATE_KEY" ]; then
    SERVICE_PRIVATE_KEY="0000000000000000000000000000000000000000000000000000000000000001"
fi

echo ""
echo "🚀 Starting Push Notification Service with Firebase"
echo "==================================================="
echo "📝 Configuration:"
echo "  - Server: http://localhost:8000"
echo "  - Relay: wss://communities.nos.social"
echo "  - Redis: localhost:6379"
echo "  - Firebase Project: ${FIREBASE_PROJECT_ID}"
echo ""
echo "🌐 Starting Plur Push Service..."
echo "================================="
echo ""

# Set environment variables and run the service
export PLUR_PUSH__SERVICE__PRIVATE_KEY_HEX=$SERVICE_PRIVATE_KEY
export PLUR_PUSH__FCM__PROJECT_ID="${FIREBASE_PROJECT_ID}"
export PLUR_PUSH__FCM__CREDENTIALS_BASE64="$CREDENTIALS_BASE64"
export REDIS_URL="redis://localhost:6379"
export RUST_LOG="debug,nostr_relay_pool=debug"

# Run the service
cargo run