#!/bin/bash

# Firebase Setup Script for Local Development
# This script helps you configure Firebase for local FCM testing

echo "🔥 Firebase Local Setup for Nostr Push Service"
echo "============================================="
echo ""
echo "Follow these steps to set up Firebase:"
echo ""
echo "1. Go to https://console.firebase.google.com/"
echo "2. Create a new project or select existing one"
echo "3. Add a Web App to your project"
echo "4. Get your Firebase configuration"
echo "5. Download service account JSON from Project Settings → Service Accounts"
echo ""
echo "Once you have the credentials, update these files:"
echo ""

# Create .env.local template if it doesn't exist
if [ ! -f .env.local ]; then
    cat > .env.local << 'EOF'
# Firebase Configuration for Local Development
# Fill in these values from your Firebase project

# From Firebase Console → Project Settings → General → Your apps
FIREBASE_API_KEY=your-api-key-here
FIREBASE_AUTH_DOMAIN=your-project.firebaseapp.com
FIREBASE_PROJECT_ID=your-project-id
FIREBASE_STORAGE_BUCKET=your-project.appspot.com
FIREBASE_MESSAGING_SENDER_ID=your-sender-id
FIREBASE_APP_ID=your-app-id

# Path to service account JSON (downloaded from Firebase Console)
FIREBASE_SERVICE_ACCOUNT_PATH=./firebase-service-account.json
EOF
    echo "✅ Created .env.local template"
else
    echo "ℹ️  .env.local already exists"
fi

echo ""
echo "📝 Important Security Notes:"
echo "  • Firebase web config (API key, etc.) = PUBLIC (safe for browser)"
echo "  • Service account JSON = PRIVATE (server-side only, never expose!)"

# Create firebase config template
cat > frontend/firebase-config.js << 'EOF'
// Firebase configuration for Web Push
// Auto-generated from environment variables
const firebaseConfig = {
    apiKey: process.env.FIREBASE_API_KEY || "your-api-key",
    authDomain: process.env.FIREBASE_AUTH_DOMAIN || "your-project.firebaseapp.com",
    projectId: process.env.FIREBASE_PROJECT_ID || "your-project-id",
    storageBucket: process.env.FIREBASE_STORAGE_BUCKET || "your-project.appspot.com",
    messagingSenderId: process.env.FIREBASE_MESSAGING_SENDER_ID || "your-sender-id",
    appId: process.env.FIREBASE_APP_ID || "your-app-id"
};
EOF

echo "✅ Created frontend/firebase-config.js template"
echo ""
echo "Next steps:"
echo "1. Edit .env.local with your Firebase credentials"
echo "2. Save your service account JSON as firebase-service-account.json"
echo "3. Run ./test_frontend_firebase.sh to start with real Firebase"
echo ""
echo "⚠️  Remember to add these files to .gitignore:"
echo "   - .env.local"
echo "   - firebase-service-account.json"