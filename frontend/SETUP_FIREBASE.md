# Setting Up Firebase for Web Push Notifications

This guide explains how to configure Firebase Cloud Messaging (FCM) for web push notifications with the Plur Push Service.

## Prerequisites

1. A Firebase project (create one at https://console.firebase.google.com)
2. The project ID should match what's configured in the backend (`pub-verse-app`)

## Step 1: Get Firebase Configuration

1. Go to [Firebase Console](https://console.firebase.google.com)
2. Select your project
3. Click the gear icon ⚙️ → **Project Settings**
4. Under **General** tab, scroll to **Your apps**
5. Click **Add app** → **Web** (</>) if you haven't already
6. Register your app with a nickname (e.g., "Plur Push Web")
7. Copy the Firebase configuration object

## Step 2: Get VAPID Key for Web Push

1. In **Project Settings** → **Cloud Messaging** tab
2. Scroll to **Web configuration** section
3. Under **Web Push certificates**, click **Generate key pair** if needed
4. Copy the **Key pair** value (this is your VAPID key)

## Step 3: Update Configuration Files

Edit `frontend/firebase-config.js`:

```javascript
const firebaseConfig = {
    apiKey: "your-api-key",
    authDomain: "your-project.firebaseapp.com",
    projectId: "pub-verse-app",  // Must match backend config
    storageBucket: "your-project.appspot.com",
    messagingSenderId: "your-sender-id",
    appId: "your-app-id",
    measurementId: "your-measurement-id"
};

const vapidKey = "your-vapid-key-from-step-2";
```

## Step 4: Test the Setup

1. Start the push service:
   ```bash
   cargo run
   ```

2. Open http://localhost:8000 in your browser

3. Click **"Get FCM Token"**
   - Browser will ask for notification permission
   - Grant permission to proceed
   - A real FCM token will be displayed

4. Click **"Register for Push"** to register the token with the service

5. Test notifications by:
   - Sending a DM to yourself
   - Creating a subscription and sending matching events
   - Having someone mention you in an event

## Development vs Production

### For Development (without Firebase):
- The frontend will detect missing Firebase config
- Falls back to test tokens automatically
- Useful for testing the Nostr integration without real push

### For Production:
- Requires valid Firebase configuration
- Generates real FCM tokens for web browsers
- Actual push notifications will be delivered

## Troubleshooting

### "Firebase not configured" message
- Check that `firebase-config.js` has real values, not placeholders
- Ensure the file is being served correctly at `/firebase-config.js`

### "Failed to register service worker"
- Service worker requires HTTPS in production (localhost is exempt)
- Check browser console for specific errors
- Ensure `/firebase-messaging-sw.js` is accessible

### Notifications not received
1. Check browser notification permissions
2. Verify token is registered (check Redis: `SMEMBERS user_tokens:<your-pubkey>`)
3. Check service logs for FCM send attempts
4. Ensure events match your subscription filters

### Token expires or changes
- FCM tokens can change when:
  - Browser data is cleared
  - App is reinstalled
  - Token refresh occurs
- The app saves tokens in localStorage for persistence
- Re-register if token changes

## Security Notes

- Never commit real Firebase credentials to version control
- Use environment variables for production deployments
- The `firebase-config.js` values are public (visible to browsers)
- Keep your FCM server credentials (service account JSON) secret