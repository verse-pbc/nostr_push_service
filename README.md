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
```

#### Credential Configuration

```bash
# LOCAL DEVELOPMENT - Point to local files:
export NOSTR_PUSH__APPS__NOSTRPUSHDEMO__CREDENTIALS_PATH="./firebase-service-account-nostrpushdemo.json"
export NOSTR_PUSH__APPS__UNIVERSES__CREDENTIALS_PATH="./firebase-service-account-universes.json"

# Or just place files with these exact names (auto-detection):
# - firebase-service-account-nostrpushdemo.json
# - firebase-service-account-universes.json

# PRODUCTION (K8s) - Point to mounted secret files:
export NOSTR_PUSH__APPS__NOSTRPUSHDEMO__CREDENTIALS_PATH="/app/secrets/firebase-nostrpushdemo.json"
export NOSTR_PUSH__APPS__UNIVERSES__CREDENTIALS_PATH="/app/secrets/firebase-universes.json"
```

### Application Configuration

Configure supported apps in `config/settings.yaml`:

```yaml
apps:
  - name: "nostrpushdemo"
    fcm_project_id: "plur-push-local"
    
  - name: "universes"
    fcm_project_id: "universes-push"  # Your Firebase project ID
```

Each app needs its own Firebase service account. See `setup-credentials.sh` for configuration options.

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