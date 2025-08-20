#!/bin/bash

echo "🚀 Starting Push Notification Test Environment"
echo "============================================"

# Check if Redis is running
if ! redis-cli ping > /dev/null 2>&1; then
    echo "⚠️  Redis is not running. Starting Redis..."
    redis-server --daemonize yes
    sleep 2
fi

echo "✅ Redis is running"

# Set environment variables for testing
export RUST_LOG=debug
export PLUR_PUSH__SERVICE__PRIVATE_KEY_HEX="0000000000000000000000000000000000000000000000000000000000000001"
export PLUR_PUSH__FCM__CREDENTIALS_BASE64="ewogICJ0eXBlIjogInNlcnZpY2VfYWNjb3VudCIsCiAgInByb2plY3RfaWQiOiAidGVzdC1wcm9qZWN0IiwKICAicHJpdmF0ZV9rZXlfaWQiOiAidGVzdCIsCiAgInByaXZhdGVfa2V5IjogIi0tLS0tQkVHSU4gUFJJVkFURSBLRVktLS0tLVxuTUlJRXZ3SUJBREFOQmdrcWhraUc5dzBCQVFFRkFBU0NCS2t3Z2dTbEFnRUFBb0lCQVFDbHVXK0VIYUJvaTB1M1xuLS0tLS1FTkQgUFJJVkFURSBLRVktLS0tLSIsCiAgImNsaWVudF9lbWFpbCI6ICJ0ZXN0QHRlc3QtcHJvamVjdC5pYW0uZ3NlcnZpY2VhY2NvdW50LmNvbSIsCiAgImNsaWVudF9pZCI6ICIxMjM0NTY3ODkwIiwKICAiYXV0aF91cmkiOiAiaHR0cHM6Ly9hY2NvdW50cy5nb29nbGUuY29tL28vb2F1dGgyL2F1dGgiLAogICJ0b2tlbl91cmkiOiAiaHR0cHM6Ly9vYXV0aDIuZ29vZ2xlYXBpcy5jb20vdG9rZW4iLAogICJhdXRoX3Byb3ZpZGVyX3g1MDlfY2VydF91cmwiOiAiaHR0cHM6Ly93d3cuZ29vZ2xlYXBpcy5jb20vb2F1dGgyL3YxL2NlcnRzIiwKICAiY2xpZW50X3g1MDlfY2VydF91cmwiOiAiaHR0cHM6Ly93d3cuZ29vZ2xlYXBpcy5jb20vcm9ib3QvdjEvbWV0YWRhdGEveDUwOS90ZXN0JTQwdGVzdC1wcm9qZWN0LmlhbS5nc2VydmljZWFjY291bnQuY29tIgp9"

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