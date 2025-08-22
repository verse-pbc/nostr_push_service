// Firebase Messaging Service Worker
// This file must be at the root of your domain for Web Push to work
// Version: 1.0.2 - Force cache update for PWA fixes

importScripts('https://www.gstatic.com/firebasejs/10.7.1/firebase-app-compat.js');
importScripts('https://www.gstatic.com/firebasejs/10.7.1/firebase-messaging-compat.js');

// Initialize Firebase - you need to update firebase-config.js with your config
self.importScripts('/firebase-config.js');

firebase.initializeApp(firebaseConfig);

const messaging = firebase.messaging();

// Cache version - increment this to force cache update
const CACHE_VERSION = 'v1.0.2';
const CACHE_NAME = `nostr-push-${CACHE_VERSION}`;

// Service worker lifecycle management
// Skip waiting and claim clients immediately when updating
self.addEventListener('install', (event) => {
    console.log('[Service Worker] Installing new version...', CACHE_VERSION);
    self.skipWaiting(); // Replace old service worker immediately
});

self.addEventListener('activate', (event) => {
    console.log('[Service Worker] Activating new version...', CACHE_VERSION);
    event.waitUntil(
        Promise.all([
            // Clean up old caches
            caches.keys().then(cacheNames => {
                return Promise.all(
                    cacheNames
                        .filter(cacheName => cacheName.startsWith('nostr-push-') && cacheName !== CACHE_NAME)
                        .map(cacheName => {
                            console.log('[Service Worker] Deleting old cache:', cacheName);
                            return caches.delete(cacheName);
                        })
                );
            }),
            // Take control of all clients immediately
            clients.claim()
        ])
    );
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

// Handle fetch events - bypass cache for HTML to ensure updates
self.addEventListener('fetch', (event) => {
    // For HTML files, always fetch fresh from network
    if (event.request.mode === 'navigate' || event.request.url.endsWith('.html')) {
        event.respondWith(
            fetch(event.request)
                .then(response => {
                    // Clone the response before using it
                    const responseToCache = response.clone();
                    
                    // Update cache with fresh response
                    caches.open(CACHE_NAME).then(cache => {
                        cache.put(event.request, responseToCache);
                    });
                    
                    return response;
                })
                .catch(() => {
                    // If network fails, try cache as fallback
                    return caches.match(event.request);
                })
        );
        return;
    }
    
    // For other resources, use cache-first strategy
    event.respondWith(
        caches.match(event.request)
            .then(response => response || fetch(event.request))
    );
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