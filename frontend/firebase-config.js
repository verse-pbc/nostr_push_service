// Firebase configuration - dynamically generated
// Use 'const' instead of window for service worker compatibility
const firebaseConfig = {
    apiKey: "AIzaSyBVZr13kC2niDhmJX2E0oMhGRlDqmC1wSA",
    authDomain: "plur-push-local.firebaseapp.com",
    projectId: "plur-push-local",
    storageBucket: "plur-push-local.firebasestorage.app",
    messagingSenderId: "103876204196",
    appId: "1:103876204196:web:d7a1a1bebbdf6e6b75b831",
    vapidPublicKey: "BPVz4oPf--UKQHFkooAq1SROioo2AmaQ_e98wwjTtsHnfhB_wV2VL1cLzgTZKl3p9c-Ueev_0BNeMIvoCUz2LrM"
};

// Also set on window if it exists (for main page)
if (typeof window !== 'undefined') {
    window.firebaseConfig = firebaseConfig;
}
