// Firebase configuration for FCM Web Push
// This is a template - replace with your actual Firebase config

const firebaseConfig = {
    apiKey: "YOUR_API_KEY",
    authDomain: "YOUR_AUTH_DOMAIN",
    projectId: "pub-verse-app",
    storageBucket: "YOUR_STORAGE_BUCKET",
    messagingSenderId: "YOUR_MESSAGING_SENDER_ID",
    appId: "YOUR_APP_ID",
    measurementId: "YOUR_MEASUREMENT_ID"
};

// VAPID key for web push (get from Firebase Console > Project Settings > Cloud Messaging > Web Push certificates)
const vapidKey = "YOUR_VAPID_KEY";