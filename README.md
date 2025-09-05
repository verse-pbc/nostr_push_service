# Nostr Push Service

A service that implements encrypted push notifications for Nostr clients following the [NIP-XX Push Notifications draft specification](docs/nip-xx-push-notifications.md).

## Overview

This service:
- Listens for encrypted push token registrations from Nostr clients (kinds 3079-3082)
- Monitors configured relays for events matching user-defined filters
- Sends push notifications via Firebase Cloud Messaging (FCM) when relevant events occur
- Supports multiple apps with isolated token management
- Requires NIP-44 encryption for all tokens (plaintext tokens are rejected)

## Protocol

The service implements four Nostr event kinds:

- **3079**: Register push token (encrypted)
- **3080**: Deregister push token (encrypted)  
- **3081**: Add/update notification filter (encrypted)
- **3082**: Delete notification filter (encrypted)

All events must include a `p` tag with the service's public key and use NIP-44 encryption. See the [protocol specification](docs/nip-xx-push-notifications.md) for details.

## Configuration

### Environment Variables

```bash
# Required
NOSTR_PUSH__SERVICE__PRIVATE_KEY_HEX="<service_private_key>"  # For NIP-44 decryption
REDIS_URL="redis://localhost:6379"                            # Token storage

# Firebase (for push delivery)
GOOGLE_APPLICATION_CREDENTIALS="/path/to/firebase-service-account.json"
```

### Application Configuration

Configure supported apps in `config/settings.yaml`:

```yaml
apps:
  - name: "myapp"
    fcm_project_id: "my-firebase-project"
```

## Running the Service

### Docker

```bash
docker compose build
docker compose up -d
```

### Development

```bash
cargo build --release
cargo run --release
```

### Testing

Visit `http://localhost:8000/` for a test interface that demonstrates:
- Token registration with NIP-44 encryption
- Subscription management
- Push notification testing

## Architecture

1. **Clients** publish encrypted registration events to Nostr relays
2. **Service** monitors relays and decrypts events targeted to its public key
3. **Redis** stores pubkey→token mappings with app isolation
4. **FCM** delivers push notifications to registered devices

## Security

- All tokens must be NIP-44 encrypted (plaintext rejected)
- Tokens are isolated by app ID to prevent cross-app access
- Invalid tokens are automatically removed on FCM errors
- Supports token transfer between pubkeys for account switching