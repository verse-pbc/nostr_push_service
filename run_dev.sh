#!/bin/bash

# Simple test script for local development
# Uses settings.development.yaml by default (APP_ENV defaults to development)

# Check if service account files exist
if [ ! -f "firebase-service-account-nostrpushdemo.json" ]; then
    echo "❌ Error: firebase-service-account-nostrpushdemo.json not found!"
    echo "Download it from Firebase Console → Project Settings → Service Accounts"
    exit 1
fi

echo "✅ Found firebase-service-account-nostrpushdemo.json"

if [ ! -f "firebase-service-account-universes.json" ]; then
    echo "⚠️  Warning: firebase-service-account-universes.json not found"
    echo "Universes app will not have push notification support"
fi

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

# Allow overriding which frontend app to serve (defaults to nostrpushdemo)
if [ -z "$FRONTEND_APP" ]; then
    FRONTEND_APP="nostrpushdemo"
fi

echo ""
echo "🚀 Starting Push Notification Service (Development Mode)"
echo "========================================================"
echo "📝 Configuration:"
echo "  - Environment: development (using settings.development.yaml)"
echo "  - Frontend App: $FRONTEND_APP"
echo "  - Server: http://localhost:8000"
echo "  - Relay: wss://communities.nos.social"
echo "  - Redis: localhost:6379"
echo ""
echo "🌐 Starting Nostr Push Service..."
echo "================================="
echo ""

# Set minimal environment variables
export NOSTR_PUSH__SERVICE__PRIVATE_KEY_HEX=$SERVICE_PRIVATE_KEY
export REDIS_URL="redis://localhost:6379"
export RUST_LOG="info"
export FRONTEND_APP

# APP_ENV defaults to development in the code, so no need to set it

# Run the service
cargo run --bin nostr_push_service