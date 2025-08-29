NIP-XX
======

Push Notifications
------------------

`draft` `optional`

Define a standard for registering push tokens and receiving notifications when clients aren't connected to relays.

## Abstract

Clients register encrypted push tokens with a push service and may post encrypted, per-app filters. Services watch relays and deliver notifications to registered devices.

## Motivation

Avoid always-on connections (battery), deliver timely alerts, isolate multiple apps, and enable secure token management through encryption.

## Specification

### Event Kinds

- `3079`: Push token registration
- `3080`: Push token deregistration
- `3081`: Notification filter upsert
- `3082`: Notification filter delete

All event content fields MUST contain NIP-44 ciphertext strings (no "nip44:" prefix). The decrypted payload structure is implementation-specific between client and service.

### Registration (kind 3079)

```json
{
  "kind": 3079,
  "pubkey": "<client-pubkey>",
  "tags": [
    ["p", "<push-service-pubkey>"],
    ["app", "<app-id>"],
    ["expiration", "<unix-seconds>"]
  ],
  "content": nip44_encrypt({"token": "<platform-token>"}),
  "sig": "<signature>"
}
```

The content field contains the NIP-44 encrypted token payload. Example plaintext structure:
```json
{ "token": "<platform-token>" }
```

**Note:** The exact payload structure is implementation-specific. Services define their own required fields.

**Rules:**
- `p`, `app`, `expiration` MUST be present.
- `content` MUST be NIP-44 ciphertext; services MUST reject plaintext.
- Expiration per NIP-40. Servers MUST ignore expired events. Clients SHOULD refresh early (30–90d).

### Deregistration (kind 3080)

Same structure and rules as 3079. Example with minimal payload:
```json
{
  "kind": 3080,
  "pubkey": "<client-pubkey>",
  "tags": [
    ["p", "<push-service-pubkey>"],
    ["app", "<app-id>"],
    ["expiration", "<unix-seconds>"]
  ],
  "content": nip44_encrypt({"token": "<platform-token>"}),
  "sig": "<signature>"
}
```

### Filter Upsert (kind 3081)

```json
{
  "kind": 3081,
  "pubkey": "<client-pubkey>",
  "tags": [
    ["p", "<push-service-pubkey>"],
    ["app", "<app-id>"],
    ["expiration", "<unix-seconds>"]
  ],
  "content": nip44_encrypt(filter_payload),
  "sig": "<signature>"
}
```

Example minimal payload (implementation-specific):

```json
{
  "filter": {
    "kinds": [1, 7, 9735],
    "#p": ["<my-pubkey-hex>"]
  }
}
```

**Rules:** same tag+expiration rules as 3079; servers MUST ignore expired filters.

### Filter Delete (kind 3082)

```json
{
  "kind": 3082,
  "pubkey": "<client-pubkey>",
  "tags": [
    ["p", "<push-service-pubkey>"],
    ["app", "<app-id>"]
  ],
  "content": nip44_encrypt(filter_payload),
  "sig": "<signature>"
}
```

Example minimal payload (implementation-specific):
```json
{
  "filter": {
    "kinds": [1, 7, 9735],
    "#p": ["<my-pubkey-hex>"]
  }
}
```

Services define how filters are identified and deleted. This is implementation specific and can be normalized and hashed for internal implementations.

## Notification Triggers

Services MAY implement fixed notification triggers (e.g., DMs, mentions) and/or dynamic triggers based on user-defined filters from kind 3081 events.

## Implementation Requirements

### Push Service

1. **Encryption**: Reject plaintext for all event kinds (3079/3080/3081/3082). Content must be valid NIP-44 ciphertext.
2. **App isolation**: Partition by app; ignore events with unknown app.
3. **Expiration**: Ignore expired events (NIP-40).
4. **Multiple devices**: Support multiple tokens per (pubkey, app).
5. **Idempotency**: At most one notification per (recipient_pubkey, app, event_id); aggregate reasons internally.
6. **Error handling**: Remove invalid tokens on provider errors.
7. **Token security**: Protect stored tokens; redact in logs.
8. **Targeting**: If `p` present and not this service's pubkey, ignore.

### Client

1. Encrypt with NIP-44 to service pubkey.
2. Follow service's documentation for required payload fields.
3. Stable app id per application.
4. Refresh before expiration; deregister on logout.
5. Verify service identity via discovery.

## Security

- **Token privacy**: Publishing {pubkey ↔ token} enables correlation; NIP-44 mitigates.
- **Replay**: Expiration (NIP-40) bounds replays.
- **Rotation**: Refresh/rotate tokens to limit exposure.
- **Isolation**: `app` prevents cross-app misuse.

## Examples

Examples use `nip44_encrypt(...)` as pseudocode; actual content MUST be the ciphertext string. Payload structures shown are examples - services define their own requirements.

### Register (JS sketch)

```javascript
// Minimal example - service defines required fields
const tokenPayload = { token: fcmToken };

const event = {
  kind: 3079,
  pubkey: myPub,
  created_at: Math.floor(Date.now() / 1000),
  tags: [
    ["p", pushServicePubkey],
    ["app", "my-nostr-app"],
    ["expiration", String(Math.floor(Date.now() / 1000) + 7776000)]
  ],
  content: await nip44.encrypt(
    pushServicePubkey, 
    myPriv, 
    JSON.stringify(tokenPayload)
  )
};
await relay.publish(await signEvent(event, myPriv));
```

### Filter upsert

```javascript
const filterPayload = {
  filter: { kinds: [1, 9], "#p": [myPub] }
};

const ev = {
  kind: 3081,
  pubkey: myPub,
  created_at: Math.floor(Date.now() / 1000),
  tags: [
    ["p", pushServicePubkey],
    ["app", "my-nostr-app"],
    ["expiration", String(Math.floor(Date.now() / 1000) + 2592000)]
  ],
  content: await nip44.encrypt(
    pushServicePubkey, 
    myPriv, 
    JSON.stringify(filterPayload)
  )
};
```

### Filter delete

```javascript 
const deletePayload = {
  filter: { kinds: [1, 9], "#p": [myPub] }  // Same filter to remove
};

const ev = {
  kind: 3082,
  pubkey: myPub,
  created_at: Math.floor(Date.now() / 1000),
  tags: [
    ["p", pushServicePubkey],
    ["app", "my-nostr-app"]
  ],
  content: await nip44.encrypt(
    pushServicePubkey, 
    myPriv, 
    JSON.stringify(deletePayload)
  )
};
```

### Service Discovery

Services can advertise their availability through various means. See [NIP-89](https://github.com/nostr-protocol/nips/blob/master/89.md) for application handler discovery patterns.