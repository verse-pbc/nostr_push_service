# Pending Enhancements for nostr_push_service

**Purpose**: Track needed improvements and bug fixes discovered during integration work
**Last Updated**: 2025-10-16

---

## Critical Issues

### 1. NIP-29 Custom Subscriptions Missing Membership Check

**Status**: ✅ **RESOLVED** (2025-10-16)
**Fixed in**: Commit e7401dc
**Severity**: HIGH (was)
**Discovered**: 2025-10-16 during Peek push notification integration planning

**Problem**:
When users subscribe to NIP-29 groups via custom filters (kind 3081), they don't receive notifications for regular messages (only mentions).

**Root Cause** (event_handler.rs:242-253, 646-743):
```rust
// Route logic
else if has_h_tag {
    return handle_group_message(state, event, token).await;  // ← Routes here
}

// handle_group_message (line 678-683)
let mentioned_pubkeys = extract_mentioned_pubkeys(event);
if mentioned_pubkeys.is_empty() {
    debug!("No mentioned pubkeys found, skipping notification.");
    return Ok(());  // ← BUG: Returns without checking custom subscriptions!
}
```

**Expected Behavior**:
1. User subscribes to NIP-29 group via filter: `{kinds: [9], #h: ["group-id"]}`
2. Regular message posted to group (kind 9, h-tag, no mentions)
3. **Should**: Check custom subscriptions, verify user is group member, send notification
4. **Actually**: Routes to handle_group_message → sees no mentions → returns → NO NOTIFICATION

**Impact**:
- Peek users who subscribe to communities won't receive notifications for general messages
- Only mentions and broadcasts work
- Custom subscription feature is broken for NIP-29 groups

**Proposed Fix** (choose one approach):

**Option A** (Recommended): Check custom subscriptions BEFORE h-tag routing
```rust
// In route_event, BEFORE tag-based routing:
if has_custom_subscribers_for_event(state, event) {
    let recipients = get_custom_subscription_recipients(state, event).await;

    // For NIP-29 groups, filter recipients to only members
    let recipients = if has_h_tag {
        filter_by_group_membership(state, event, recipients).await
    } else {
        recipients
    };

    send_notifications_to_recipients(state, event, recipients, token).await?;
}

// Then continue with tag-based routing for mentions/broadcasts
```

**Option B**: Extend handle_group_message to check custom subscriptions
```rust
// In handle_group_message, AFTER mention handling:
if mentioned_pubkeys.is_empty() {
    // No mentions - check if anyone subscribed via custom filter
    let subscribers = get_users_with_matching_subscriptions(state, event).await?;

    for subscriber in subscribers {
        // Verify subscriber is a group member
        if is_group_member(group_id, &subscriber).await? {
            send_notification_to_user(state, event, &subscriber, token).await?;
        }
    }
}
```

**Option C**: Add NIP-29 membership check to handle_custom_subscriptions
```rust
// In handle_custom_subscriptions, when matching filter:
if filter.match_event(event) {
    // Check if event is NIP-29 group event
    if let Some(group_id) = event.tags.find(TagKind::h()).and_then(|t| t.content()) {
        // Verify user is member of this group
        if !state.nip29_client.is_group_member(group_id, &user_pubkey).await? {
            debug!("User not member of group, skipping");
            continue;
        }
    }

    // Send notification (user is member or it's not a group event)
    send_notification_to_user_for_app(state, event, &user_pubkey, &app, token).await?;
}
```

**Solution Implemented**:
Added custom subscription checking directly in `handle_group_message` after mention/broadcast handling.
- Users can now subscribe via `{kinds: [9], #h: ["group-id"]}`
- Group membership validated before sending notifications
- ~80 lines of subscription logic added to handle_group_message
- Note: Creates code duplication with handle_custom_subscriptions - potential future refactor

**Testing**:
```bash
# Create test for this scenario
cd /Users/daniel/code/nos/nostr_push_service

# Test case:
# 1. User subscribes with filter {kinds: [9], #h: ["test-group"]}
# 2. Post kind 9 message with h-tag but NO p-tag mention
# 3. Verify user receives notification
# 4. Ban user from group (via NIP-29 moderation)
# 5. Post another message
# 6. Verify user does NOT receive notification (membership check working)
```

**References**:
- event_handler.rs:242-253 (routing logic)
- event_handler.rs:646-743 (handle_group_message)
- event_handler.rs:1178-1405 (handle_custom_subscriptions)

---

## Code Quality

### 2. Refactor Subscription Logic Duplication

**Status**: ✅ **RESOLVED** (2025-10-16)
**Severity**: MEDIUM (was)
**Created**: 2025-10-16 during NIP-29 fix implementation

**Problem**:
Subscription checking logic was duplicated between `handle_group_message` and `handle_custom_subscriptions`.

**Solution**:
Extracted into helper function `check_subscriptions_for_matching_users(group_id: Option<&str>)`
- When group_id is Some: validates NIP-29 group membership
- When group_id is None: skips membership check
- Eliminated ~80 lines of code duplication
- Single source of truth for subscription matching logic

---

## Enhancement Requests

### 3. Automatic Cleanup on NIP-29 Ban Events

**Status**: ✅ **RESOLVED** (2025-10-16)
**Severity**: MEDIUM (was)
**Discovered**: 2025-10-16 during Peek integration planning

**Problem**:
When users are banned from NIP-29 groups, their subscriptions persist until they manually unsubscribe.

**Current Behavior**:
- Peek validation service publishes NIP-29 moderation event (kind 9000 - remove-user)
- NIP-29 relay removes user from group membership
- nostr_push_service doesn't monitor kind 9000 events
- User's subscription (kind 3081) remains in Redis
- **With fix #1 above**: Membership check prevents notifications (good)
- **Without fix #1**: User might still receive notifications (bad)

**Enhancement**:
Monitor NIP-29 moderation events and auto-cleanup subscriptions:

```rust
// Add to service.control_kinds in config
control_kinds: [3079, 3080, 3081, 3082, 9000, 9001, 9002]  // Add NIP-29 moderation

// In handle_nip29_moderation (new handler):
async fn handle_nip29_moderation(state: &AppState, event: &Event) -> Result<()> {
    match event.kind.as_u16() {
        9000 => {  // remove-user
            let group_id = event.tags.find(TagKind::h())?.content()?;
            let removed_pubkey = event.tags.find(TagKind::p())?.content()?;

            // Find and remove all subscriptions for this user in this group
            let filter_to_remove = {kinds: [9], #h: [group_id]};
            remove_subscription_by_filter(state, removed_pubkey, filter_to_remove).await?;
        }
        // Handle other moderation events as needed
        _ => {}
    }
    Ok(())
}
```

**Solution Implemented**:
Lazy cleanup on membership check failure:
- When membership check fails during subscription processing, check if filter is specific to that group
- If filter has exactly one h-tag matching the failed group, safely remove subscription
- If filter targets multiple groups or has no h-tag, keep it (might be valid for other groups/events)
- Self-healing without monitoring moderation events
- Zero added complexity - cleanup happens where membership is already checked

---

## Documentation Improvements

### 4. Document NIP-29 Relay Requirements

**Status**: 🟢 **DOCUMENTATION**
**Severity**: LOW

Add section to README.md explaining NIP-29 relay integration:
- Relay must implement NIP-29 group membership queries
- Service requires `is_group_member()` and `get_group_members()` APIs
- How membership state is cached/refreshed

---

## Future Considerations

### 5. Rate Limiting per Group

**Status**: 🔵 **FUTURE**
**Severity**: LOW
**Note**: Deferred from Peek integration

Could add per-group rate limiting to prevent notification spam:
- Max N notifications per group per user per hour
- Configurable threshold in settings.yaml
- Track in Redis: `rate_limit:{app}:{pubkey}:{group_id}` with TTL

---

## Change Log

| Date | Issue | Status | Notes |
|------|-------|--------|-------|
| 2025-10-16 | #1 NIP-29 custom subscription membership check | ✅ Resolved | Fixed in commit e7401dc |
| 2025-10-16 | #2 Subscription logic duplication | ✅ Resolved | Refactored with helper function |
| 2025-10-16 | #3 Auto-cleanup on ban events | ✅ Resolved | Lazy cleanup on membership failure |

---

## How to Use This Document

1. **Before starting work on nostr_push_service**, review this file
2. **When discovering new issues**, add them with status, severity, and context
3. **When implementing fixes**, update status and add reference to PR/commit
4. **Archive resolved items** by moving to a separate CHANGELOG.md

---

**Maintainer**: Daniel Cadenas
**Related Projects**: Peek (location-based communities), Universes
