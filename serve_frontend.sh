#!/bin/bash

# Load Firebase configuration from environment or .env.local
if [ -f .env.local ]; then
    export $(grep -v '^#' .env.local | xargs)
fi

# Create a temporary firebase-config.js with actual values
cat > frontend/firebase-config.js <<EOF
// Firebase configuration - dynamically generated
window.firebaseConfig = {
    apiKey: "${FIREBASE_API_KEY}",
    authDomain: "${FIREBASE_AUTH_DOMAIN}",
    projectId: "${FIREBASE_PROJECT_ID}",
    storageBucket: "${FIREBASE_STORAGE_BUCKET}",
    messagingSenderId: "${FIREBASE_MESSAGING_SENDER_ID}",
    appId: "${FIREBASE_APP_ID}"
};
EOF

echo "✅ Generated firebase-config.js with environment values"
echo "📱 Firebase Project: ${FIREBASE_PROJECT_ID}"
echo "🌐 Starting frontend server on http://localhost:8080"

# Start Python HTTP server
cd frontend && python3 -m http.server 8080