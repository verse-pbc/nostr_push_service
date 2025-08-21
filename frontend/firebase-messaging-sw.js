// Firebase Messaging Service Worker
// This file must be at the root of your domain for Web Push to work
// Version: 1.0.1

importScripts('https://www.gstatic.com/firebasejs/10.7.1/firebase-app-compat.js');
importScripts('https://www.gstatic.com/firebasejs/10.7.1/firebase-messaging-compat.js');

// Initialize Firebase - you need to update firebase-config.js with your config
self.importScripts('/firebase-config.js');

firebase.initializeApp(firebaseConfig);

const messaging = firebase.messaging();

// Service worker lifecycle management
// Skip waiting and claim clients immediately when updating
self.addEventListener('install', (event) => {
    console.log('[Service Worker] Installing new version...');
    self.skipWaiting(); // Replace old service worker immediately
});

self.addEventListener('activate', (event) => {
    console.log('[Service Worker] Activating new version...');
    event.waitUntil(clients.claim()); // Take control of all clients immediately
});

// Handle background messages - this is the proper FCM way for data-only messages
messaging.onBackgroundMessage((payload) => {
    console.log('[firebase-messaging-sw.js] Received background message', payload);
    console.log('[firebase-messaging-sw.js] Payload data:', payload.data);
    console.log('[firebase-messaging-sw.js] Title from data:', payload.data?.title);
    console.log('[firebase-messaging-sw.js] Body from data:', payload.data?.body);
    
    // We're using data-only messages
    if (payload.data && payload.data.title && payload.data.body) {
        const title = payload.data.title;
        // Add service worker scope/URL info to the body
        const swInfo = `[SW: ${self.registration.scope}]`;
        const body = `${payload.data.body}\n${swInfo}`;
        
        console.log('[firebase-messaging-sw.js] Creating notification with:', { title, body });
        console.log('[firebase-messaging-sw.js] Service Worker Scope:', self.registration.scope);
        
        const notificationOptions = {
            body: body,
            icon: '/icon-192x192.png',
            badge: '/badge-72x72.png',
            data: {
                ...payload.data,
                serviceWorkerScope: self.registration.scope,
                serviceWorkerURL: self.location.href
            },
            tag: payload.data.nostrEventId || `nostr-${Date.now()}`,
            requireInteraction: false,
        };
        
        // This is the proper way - return the promise
        return self.registration.showNotification(title, notificationOptions);
    } else {
        console.error('[firebase-messaging-sw.js] Missing required data fields');
    }
});

// Handle notification click
self.addEventListener('notificationclick', (event) => {
    console.log('[firebase-messaging-sw.js] Notification click received');
    event.notification.close();
    
    // Use data.link if available, otherwise use the service worker's scope
    const targetUrl = event.notification?.data?.link || self.registration.scope;

    // Open the app or focus existing window
    event.waitUntil(
        (async () => {
            const clientList = await clients.matchAll({ 
                type: 'window', 
                includeUncontrolled: true 
            });
            
            // Try to find and focus an existing window within our scope
            for (const client of clientList) {
                if (targetUrl && client.url.startsWith(self.registration.scope)) {
                    await client.focus();
                    // Send a message to the client about the notification
                    if (event.notification.data) {
                        client.postMessage({
                            type: 'notification-clicked',
                            data: event.notification.data
                        });
                    }
                    return;
                }
            }
            
            // If no existing window, open a new one
            if (clients.openWindow) {
                await clients.openWindow(targetUrl);
            }
        })()
    );
});