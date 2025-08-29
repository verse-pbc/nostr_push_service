# Nostr Push Service

This service listens to a Nostr relay for specific events related to user device registration and messages, and sends push notifications via Firebase Cloud Messaging (FCM) accordingly.

It is written in Rust, uses Redis for storing mappings between user public keys and device tokens, and leverages the `nostr-sdk` and `firebase-messaging-rs` crates.

> **🔒 Security Notice**: This service requires **mandatory NIP-44 encryption** for all push notification tokens. Plaintext tokens are rejected.
> 
> **🔄 Token Transfer**: Tokens can now be transferred between Nostr accounts. When a new pubkey registers an existing token, ownership automatically transfers from the previous pubkey, enabling account switching and key recovery.

## 📚 Documentation

- **[NIP-44 Encryption Guide](docs/NIP44_ENCRYPTION.md)** - Complete encryption protocol specification
- **[Developer Setup](docs/DEVELOPER_SETUP.md)** - Quick integration guide for app developers  
- **[Migration Guide](docs/MIGRATION_GUIDE.md)** - Upgrading from plaintext tokens
- **[Developer Guide](docs/developer-guide.md)** - In-depth technical documentation

## Architecture

The system design involves several components:

1.  **Client (Nostr App):**
    *   Obtains an FCM device token.
    *   Encrypts the token using NIP-44 to the service's public key.
    *   Publishes encrypted registration (`kind: 3079`) and deregistration (`kind: 3080`) events to the Nostr Relay with required tags (`p`, `app`, `expiration`).
2.  **Nostr Relay:**
    *   Acts as the communication bus between Clients and the Service.
3.  **Nostr Push Service (This Repository):**
    *   Connects to the Nostr Relay (authenticating using `service.service_private_key_hex` if provided for NIP-42).
    *   Filters events by `p` tag matching the service's public key.
    *   Listens for encrypted registration (`kind: 3079`) and deregistration (`kind: 3080`) events.
    *   Decrypts NIP-44 encrypted payloads using the service's private key.
    *   Stores/Removes mappings of `pubkey -> [fcm_token]` in Redis with app namespace isolation.
    *   Listens for relevant message events (e.g., `kind: 11`, `kind: 12`, configured via `listen_kinds`) on the relay.
    *   Determines if a push notification should be sent based on event content (e.g., user mentions).
    *   Fetches the relevant FCM tokens for the recipient pubkey(s) from Redis.
    *   Sends push notification payloads to the Firebase Cloud Messaging (FCM) server.
    *   Handles FCM errors (e.g., `NotRegistered`, `InvalidRegistration`) by immediately removing the invalid token from Redis.
    *   Periodically cleans up stale tokens from Redis based on a configurable TTL (`token_max_age_days`).
    *   Processes historical events upon startup to handle potential downtime.
4.  **Firebase Cloud Messaging (FCM):**
    *   Receives notification requests from the Nostr Push Service.
    *   Delivers push notifications to the registered client devices (iOS, Android, Web).

### Notification Types

The service supports two types of notifications:

1. **Mention-based Notifications**: When a group message mentions specific users (with 'p' tags), notifications are sent only to those mentioned users.

2. **Broadcast Notifications**: When a group message includes a 'broadcast' tag, notifications are sent to all members of the group, regardless of mentions.

### Broadcast Usage Example

To send a broadcast notification to all members of a group, include a 'broadcast' tag in your Nostr event:

```javascript
// Example using nostr-tools
const broadcastEvent = {
  kind: 11, // Must be kind 11 or 12
  content: 'Important announcement for all group members!',
  tags: [
    ['h', 'your-group-id'],
    ['broadcast'] // This tag triggers broadcast to all members
  ]
};

// Only group admins can send broadcast messages
// Sign and publish using your admin keypair
const signedEvent = await window.nostr.signEvent(broadcastEvent);
await relay.publish(signedEvent);
```

Requirements for broadcast notifications:
- The sender must be a group admin
- The event must have kind 11 or 12 (group messages)
- The event must include a 'broadcast' tag

## Configuration

The service is configured via environment variables and a `config/settings.yaml` file. Key settings include:

*   `server.listen_addr`: The address and port for the HTTP server (e.g., health check). Defaults to `0.0.0.0:8000`. (Env: `NOSTR_PUSH__SERVER__LISTEN_ADDR`)
*   `nostr.relay_url`: The URL of the Nostr relay to connect to.
*   `service.service_private_key_hex` (**Required**, field name `service_private_key_hex`, overridden by env `NOSTR_PUSH__SERVICE__PRIVATE_KEY_HEX`): Private key for the service's Nostr identity. This key serves multiple purposes:
    - Signs events for NIP-42 authentication if the relay requires it
    - Decrypts NIP-44 encrypted token payloads from clients
    - Identifies the service via `p` tags for multi-service support
*   `service.listen_kinds`: List of Nostr event kinds to monitor for sending notifications.
*   `service.process_window_days`: How far back to look for historical events on startup.
*   `redis.url` (Typically set via `NOSTR_PUSH__REDIS__URL` env var): Connection URL for the Redis instance.
*   `fcm.project_id`: Your Firebase/Google Cloud project ID.
*   `cleanup.enabled`: Whether to enable periodic stale token cleanup.
*   `cleanup.interval_secs`: How often the cleanup task runs.
*   `cleanup.token_max_age_days`: Maximum age for a token before it's considered stale.

**Authentication:**

*   **Nostr Relay:** If `service.service_private_key_hex` is set, the service uses this keypair as its identity when connecting to the relay. The `nostr-sdk` client will handle NIP-42 `AUTH` automatically if the relay requests authentication from this public key.
*   **FCM:** Authentication uses Google Application Default Credentials (ADC). Set the `GOOGLE_APPLICATION_CREDENTIALS` environment variable to the path of your service account JSON key file.

## Quick Start - Service Setup

### 1. Generate Service Keypair

```bash
# Using OpenSSL (available on most systems)
openssl rand -hex 32

# Or using a Nostr tool
npm install -g @nostr/tools
nostr-tools generate-keypair
```

### 2. Set Environment Variables

```bash
export NOSTR_PUSH__SERVICE__PRIVATE_KEY_HEX="your_generated_private_key"
export NOSTR_PUSH__REDIS__URL="redis://localhost:6379"
export NOSTR_PUSH__FCM__PROJECT_ID="your-firebase-project"
export GOOGLE_APPLICATION_CREDENTIALS="/path/to/firebase-service-account.json"
```

### 3. Access Web Interface

Visit `http://localhost:8080/` to:
- View the service's public key (npub)
- Test push notification registration with automatic NIP-44 encryption
- Subscribe to notification filters

## Running the Service

### Using Docker (Recommended)

1.  **Create a Service Account Key:** Follow Google Cloud documentation to create a service account with the `Firebase Cloud Messaging API Admin` role (or similar) and download the JSON key file.
2.  **Build the Docker Image:**
    ```bash
    docker compose build --no-cache
    ```
3.  **Run the Container:**
    Ensure your environment variables are set (e.g., in a `.env` file) for `GOOGLE_APPLICATION_CREDENTIALS`, `NOSTR_PUSH__REDIS__URL`, `NOSTR_PUSH__SERVICE__PRIVATE_KEY_HEX`, etc.
    ```bash
    docker compose up -d
    ```

4.  **Check Health:**
    Once running, you can check the service's health endpoint:
    ```bash
    curl http://localhost:8000/health
    ```
    It should return `OK`.

### Using Cargo (Development)

1.  **Build the Service:**
    ```bash
    cargo build --release
    ```
2.  **Run the Service:**
    ```bash
    cargo run --release
    ```