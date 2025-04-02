# Plur Push Service

This service listens to a Nostr relay for specific events related to user device registration and messages, and sends push notifications via Firebase Cloud Messaging (FCM) accordingly.

It is written in Rust, uses Redis for storing mappings between user public keys and device tokens, and leverages the `nostr-sdk` and `firebase-messaging-rs` crates.

## Architecture

The system design involves several components:

1.  **Client (Plur App):**
    *   Obtains an FCM device token.
    *   Publishes registration (`kind: 3079`) and deregistration (`kind: 3080`) events to the Nostr Relay.
2.  **Nostr Relay:**
    *   Acts as the communication bus between Clients and the Service.
3.  **Plur Push Service (This Repository):**
    *   Connects to the Nostr Relay (authenticating using `service.service_private_key_hex` if provided for NIP-42).
    *   Listens for registration (`kind: 3079`) and deregistration (`kind: 3080`) events.
    *   Stores/Removes mappings of `pubkey -> [fcm_token]` in Redis.
    *   Listens for relevant message events (e.g., `kind: 11`, `kind: 12`, configured via `listen_kinds`) on the relay.
    *   Determines if a push notification should be sent based on event content (e.g., user mentions).
    *   Fetches the relevant FCM tokens for the recipient pubkey(s) from Redis.
    *   Sends push notification payloads to the Firebase Cloud Messaging (FCM) server.
    *   Handles FCM errors (e.g., `NotRegistered`, `InvalidRegistration`) by immediately removing the invalid token from Redis.
    *   Periodically cleans up stale tokens from Redis based on a configurable TTL (`token_max_age_days`).
    *   Processes historical events upon startup to handle potential downtime.
4.  **Firebase Cloud Messaging (FCM):**
    *   Receives notification requests from the Plur Push Service.
    *   Delivers push notifications to the registered client devices (iOS, Android, Web).

## Configuration

The service is configured via environment variables and a `config/settings.yaml` file. Key settings include:

*   `nostr.relay_url`: The URL of the Nostr relay to connect to.
*   `service.service_private_key_hex` (Optional, field name `service_private_key_hex`, overridden by env `PLUR_PUSH__SERVICE__PRIVATE_KEY_HEX`): Private key for the service's Nostr identity. This key is used as the signer for the Nostr client, which enables NIP-42 authentication if the relay requires it from this public key.
*   `service.listen_kinds`: List of Nostr event kinds to monitor for sending notifications.
*   `service.process_window_days`: How far back to look for historical events on startup.
*   `redis.url` (Typically set via `PLUR_PUSH__REDIS__URL` env var): Connection URL for the Redis instance.
*   `fcm.project_id`: Your Firebase/Google Cloud project ID.
*   `cleanup.enabled`: Whether to enable periodic stale token cleanup.
*   `cleanup.interval_secs`: How often the cleanup task runs.
*   `cleanup.token_max_age_days`: Maximum age for a token before it's considered stale.

**Authentication:**

*   **Nostr Relay:** If `service.service_private_key_hex` is set, the service uses this keypair as its identity when connecting to the relay. The `nostr-sdk` client will handle NIP-42 `AUTH` automatically if the relay requests authentication from this public key.
*   **FCM:** Authentication uses Google Application Default Credentials (ADC). Set the `GOOGLE_APPLICATION_CREDENTIALS` environment variable to the path of your service account JSON key file.

## Running the Service

### Using Docker (Recommended)

1.  **Create a Service Account Key:** Follow Google Cloud documentation to create a service account with the `Firebase Cloud Messaging API Admin` role (or similar) and download the JSON key file.
2.  **Build the Docker Image:**
    ```bash
    docker compose build --no-cache
    ```
3.  **Run the Container:**
    Ensure your environment variables are set (e.g., in a `.env` file) for `GOOGLE_APPLICATION_CREDENTIALS`, `PLUR_PUSH__REDIS__URL`, `PLUR_PUSH__SERVICE__PRIVATE_KEY_HEX`, etc.
    ```bash
    docker compose up -d
    ```