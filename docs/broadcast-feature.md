# Broadcast Feature for Plur Push Service

## Overview

This feature adds support for broadcasting push notifications to all members of a group when an event contains a 'broadcast' tag. Unlike regular mentions which only notify specific users tagged with 'p', broadcast notifications will be sent to every member of the group.

## Current Behavior

Currently, the Plur Push Service:
1. Listens for configured event kinds (11, 12, etc.)
2. For group messages, extracts the group ID from 'h' tag
3. Finds pubkeys mentioned in 'p' tags
4. Verifies mentioned users are group members
5. Sends notifications only to mentioned group members

## Desired Behavior

With the broadcast feature:
1. Detect if an event has a 'broadcast' tag
2. If a broadcast tag is present, verify the sender is a group admin
3. Verify the event kind is in the allowed list of broadcastable kinds (11, 12)
4. Retrieve all members of the group
5. Send push notifications to all group members, regardless of whether they're mentioned in 'p' tags
6. Maintain existing mention-based notification behavior for non-broadcast events

## Implementation Plan

### 1. Code Exploration (Completed)
- [x] Understand the current event flow
- [x] Identify key components to modify

### 2. Implementation Tasks
- [x] Modify `handle_group_message` in event_handler.rs to check for 'broadcast' tag
- [x] Add function to retrieve full group membership from NIP-29 client
- [x] Add function to verify sender is a group admin
- [x] Add a constant for allowed broadcastable event kinds (11, 12)
- [x] Implement logic to send notifications to all group members when broadcast tag is present
- [x] Add appropriate logging for broadcast events
- [x] Ensure backward compatibility with existing notification logic

### 3. Testing
- [x] Test with events containing 'broadcast' tag
- [x] Verify notifications are sent to all group members
- [x] Test regular mention-based notifications still work
- [x] Verify performance with large groups
- [x] Test admin permission verification
- [x] Test event kind filtering

### 4. Documentation
- [x] Update README.md to describe the broadcast feature
- [x] Document any configuration changes
- [x] Add usage examples

## Technical Details

### API Changes
- [x] Made `Nip29Client::get_group_members` method public

### Database Impact
- No database schema changes required
- Potential increase in Redis read operations to fetch more tokens

### Performance Considerations
- Broadcasting to large groups may increase FCM API usage
- Consider implementing rate limiting or batching for large groups
- Monitor memory usage when processing broadcasts to many recipients

## Implementation Details

### Broadcast Detection
The service now detects 'broadcast' tags in the event using this logic:
```rust
let is_broadcast = event.tags.find(TagKind::custom("broadcast")).is_some();
```

### Allowed Event Kinds
Only specific event kinds can be broadcasted:
```rust
const BROADCASTABLE_EVENT_KINDS: [u64; 2] = [11, 12];
```

### Admin Verification
Before broadcasting, the service verifies the sender is a group admin:
```rust
// Pseudocode
let is_admin = nip29_client.is_user_admin(group_id, sender_pubkey).await?;
if !is_admin {
    info!("Broadcast attempt by non-admin user {} rejected", sender_pubkey);
    return Ok(());
}
```

### Notification Handling
When a broadcast tag is detected, the service:
1. Verifies the sender is a group admin
2. Verifies the event kind is broadcastable
3. Retrieves all group members using `Nip29Client::get_group_members`
4. Sends notifications to all members (except the sender)
5. Provides detailed logging about the broadcast process

### Code Organization
The implementation extracts common notification logic into a separate `send_notification_to_user` function to avoid duplication between broadcast and mention-based notifications.

## Progress Tracking

- [x] Initial planning document (this file)
- [x] Implementation
- [x] Code review
- [x] Testing
- [x] Documentation updates
- [ ] Feature deployment