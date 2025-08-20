#!/bin/bash

echo "🚀 Starting Push Notification Test Environment"
echo "============================================"

# Load Firebase config from .env.local if it exists
if [ -f .env.local ]; then
    export $(grep -v '^#' .env.local | xargs)
    echo "✅ Loaded Firebase configuration from .env.local"
fi

# Check if Redis is running
if ! redis-cli ping > /dev/null 2>&1; then
    echo "⚠️  Redis is not running. Starting Redis..."
    redis-server --daemonize yes
    sleep 2
fi

echo "✅ Redis is running"

# Set environment variables for testing
export RUST_LOG=debug

# Service private key (required for NIP-29 client)
export PLUR_PUSH__SERVICE__PRIVATE_KEY_HEX="0000000000000000000000000000000000000000000000000000000000000001"

# Nostr relay settings (optional, defaults to wss://communities.nos.social)
# export PLUR_PUSH__NOSTR__RELAY_URL="wss://communities.nos.social"

# Redis URL (optional, defaults to redis://127.0.0.1:6379)
# export PLUR_PUSH__REDIS__URL="redis://127.0.0.1:6379"

# FCM Project ID
export PLUR_PUSH__FCM__PROJECT_ID="pub-verse-app"

# Mock FCM credentials for testing (base64 encoded service account JSON)
# This is a dummy credential that allows the service to start without real FCM
export PLUR_PUSH__FCM__CREDENTIALS_BASE64="ewogICJ0eXBlIjogInNlcnZpY2VfYWNjb3VudCIsCiAgInByb2plY3RfaWQiOiAicHViLXZlcnNlLWFwcCIsCiAgInByaXZhdGVfa2V5X2lkIjogInRlc3QiLAogICJwcml2YXRlX2tleSI6ICItLS0tLUJFR0lOIFBSSVZBVEUgS0VZLS0tLS1cbk1JSUV2d0lCQURBTkJna3Foa2lHOXcwQkFRRUZBQVNDQktrd2dnU2xBZ0VBQW9JQkFRQ2x1VytFSGFCb2kwdTNcbmVBN3o3ZU5Ub0JIcWdJL2o5MWdLeUo5TjRqN3Z2UzdheEY5YkZNQXl0cU9lMmRxZkVBYUVuVFpIcjUrWHBJZ3RcbmpSRis1SG9wcjVFN3NPTXZoOE1wR2szL3gySGg0V01MRHdCYTlOaFIrbnFwZGJHb21ybmNYQ1NsUUQrYXkrQnZcbjQyMGZPRFJYa2JqRUVINmpoSkRhK0VnMDJrQkdVMnlaV2VTUHpIMGh1TGh3eW10bVBsaStWMW9IZzRGN0xrbGRcblZzcnRJRndXejk2TlN5NWJOYWhlOFpQM0JtelJKNy9vQ1pqeUgwcjVockJGcWtHQ0kwVEFBSm01azlTNnZCUWtcbjRBQXdpQTQxSkFQcmhQdGhZL0pqYnZqRlVGRVRHUDVMV0pzRzVWbFNKVVh0RXFJRWp3b3J0Y29haHlwdEdjeWtcbjFRaFo5Vm05QWdNQkFBRUNnZ0VBRGZIaFB5VklPT3FzZ1JVcUpqWGcwM0VqbE5ZTWpiM2lMK3pzaHFJQzRJTjRcbkJvVWhkRndFS2UxL29mZXlTaE9RdzhxMU9wMVQ3Ykxucm9vSTArN0w2d3gwNUR5NnpSQnc2WHlPMG9iNFdTN1Vcbk1tK0xwL2hFSHQrNUF0N1RYUzlJMXN0eWJOYUdGNWhINWZVRzV6UzVRRnYyUlRUYjRNaFNNRllnaGUrYUJGaHJcbjhSSGJBR2ZBQUJnZUFBNGdBQUJnWUFBNGdBQUJnWUFBNGdBQUJnWUFBNGdBQUJnWUFBNGdBPT1cbi0tLS0tRU5EIFBSSVZBVEUgS0VZLS0tLS0iLAogICJjbGllbnRfZW1haWwiOiAidGVzdEBwdWItdmVyc2UtYXBwLmlhbS5nc2VydmljZWFjY291bnQuY29tIiwKICAiY2xpZW50X2lkIjogIjEyMzQ1Njc4OTAiLAogICJhdXRoX3VyaSI6ICJodHRwczovL2FjY291bnRzLmdvb2dsZS5jb20vby9vYXV0aDIvYXV0aCIsCiAgInRva2VuX3VyaSI6ICJodHRwczovL29hdXRoMi5nb29nbGVhcGlzLmNvbS90b2tlbiIsCiAgImF1dGhfcHJvdmlkZXJfeDUwOV9jZXJ0X3VybCI6ICJodHRwczovL3d3dy5nb29nbGVhcGlzLmNvbS9vYXV0aDIvdjEvY2VydHMiLAogICJjbGllbnRfeDUwOV9jZXJ0X3VybCI6ICJodHRwczovL3d3dy5nb29nbGVhcGlzLmNvbS9yb2JvdC92MS9tZXRhZGF0YS94NTA5L3Rlc3QlNDBwdWItdmVyc2UtYXBwLmlhbS5nc2VydmljZWFjY291bnQuY29tIgp9"

echo "📝 Configuration:"
echo "  - Server: http://localhost:8000"
echo "  - Relay: wss://communities.nos.social"
echo "  - Redis: localhost:6379"
echo ""
echo "🌐 Starting Plur Push Service..."
echo "================================="
echo ""

cargo run

echo ""
echo "👋 Service stopped"