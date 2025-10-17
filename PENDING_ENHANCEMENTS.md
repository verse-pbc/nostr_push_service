# Pending Enhancements for nostr_push_service

**Purpose**: Track needed improvements and future considerations
**Last Updated**: 2025-10-16

---

## Future Considerations

### Rate Limiting per Group

**Status**: 🔵 **FUTURE**
**Severity**: LOW
**Note**: Deferred from Peek integration

Could add per-group rate limiting to prevent notification spam:
- Max N notifications per group per user per hour
- Configurable threshold in settings.yaml
- Track in Redis: `rate_limit:{app}:{pubkey}:{group_id}` with TTL

---

## Change Log - Resolved Issues (2025-10-16)

| # | Issue | Commit | Summary |
|---|-------|--------|---------|
| 1 | NIP-29 custom subscriptions | e7401dc | Fixed: Users can now subscribe to group messages; membership validated |
| 2 | Subscription logic duplication | 81af681 | Refactored: Extracted helper function; eliminated ~80 lines duplication |
| 3 | Orphaned group subscriptions | 79a2472 | Fixed: Lazy cleanup on membership failure; only removes group-specific filters |
| 4 | NIP-29 documentation | README | Added: NIP-29 Group Support section with requirements |

---

## How to Use This Document

1. **Before starting work**: Review this file for context
2. **When discovering issues**: Add them with status and severity
3. **When implementing fixes**: Update resolved section with commit reference
4. **Archive old items**: Move to CHANGELOG.md when list gets large

---

**Maintainer**: Daniel Cadenas
**Related Projects**: Peek (location-based communities), Universes
