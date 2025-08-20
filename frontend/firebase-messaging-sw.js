// Firebase Messaging Service Worker
// This file must be at the root of your domain for Web Push to work

importScripts('https://www.gstatic.com/firebasejs/10.7.1/firebase-app-compat.js');
importScripts('https://www.gstatic.com/firebasejs/10.7.1/firebase-messaging-compat.js');

// Initialize Firebase - you need to update firebase-config.js with your config
self.importScripts('/firebase-config.js');

firebase.initializeApp(firebaseConfig);

const messaging = firebase.messaging();

// Handle background messages (when app is not in focus or browser is closed)
messaging.onBackgroundMessage((payload) => {
    console.log('[firebase-messaging-sw.js] Received background message ', payload);
    
    // Since we're using data-only messages, extract title and body from data
    const title = payload.data?.title || 'New Nostr Event';
    const body = payload.data?.body || 'You have a new notification';
    
    // Educational: This runs when the tab is not active or browser is minimized
    const notificationTitle = `[BACKGROUND] ${title}`;
    const notificationOptions = {
        body: `This notification was received while the app was in the background or closed.\n\n${body}`,
        icon: '/icon-192x192.png',
        badge: '/badge-72x72.png',
        data: payload.data,
        tag: payload.data?.nostrEventId || 'nostr-background-notification',
        requireInteraction: true, // Stays until user interacts
    };

    self.registration.showNotification(notificationTitle, notificationOptions);
});

// Handle notification click
self.addEventListener('notificationclick', (event) => {
    console.log('[firebase-messaging-sw.js] Notification click Received.');
    event.notification.close();

    // Open the app or focus existing window
    event.waitUntil(
        clients.matchAll({ type: 'window', includeUncontrolled: true }).then((clientList) => {
            for (const client of clientList) {
                if (client.url === '/' && 'focus' in client) {
                    return client.focus();
                }
            }
            if (clients.openWindow) {
                return clients.openWindow('/');
            }
        })
    );
});