use nostr_sdk::prelude::*;
use plur_push_service::{event_handler, redis_store};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn test_replay_horizon_ignores_old_events() {
    // Create an event that's 8 days old (beyond 7-day horizon)
    let old_timestamp = Timestamp::now() - Duration::from_secs(8 * 24 * 60 * 60);
    
    let event = EventBuilder::new(Kind::TextNote, "Old message")
        .custom_created_at(old_timestamp)
        .sign(&Keys::generate())
        .await
        .unwrap();
    
    // The event should be rejected due to age
    assert!(event_handler::is_event_too_old(&event));
}

#[tokio::test]
async fn test_replay_horizon_accepts_recent_events() {
    // Create an event that's 1 day old (within 7-day horizon)
    let recent_timestamp = Timestamp::now() - Duration::from_secs(1 * 24 * 60 * 60);
    
    let event = EventBuilder::new(Kind::TextNote, "Recent message")
        .custom_created_at(recent_timestamp)
        .sign(&Keys::generate())
        .await
        .unwrap();
    
    // The event should be accepted
    assert!(!event_handler::is_event_too_old(&event));
}

#[tokio::test]
async fn test_replay_horizon_edge_case() {
    // Create an event that's exactly 7 days old (at the horizon boundary)
    let boundary_timestamp = Timestamp::now() - Duration::from_secs(7 * 24 * 60 * 60);
    
    let event = EventBuilder::new(Kind::TextNote, "Boundary message")
        .custom_created_at(boundary_timestamp)
        .sign(&Keys::generate())
        .await
        .unwrap();
    
    // The event should still be accepted (7 days is the cutoff, not 6)
    assert!(!event_handler::is_event_too_old(&event));
}

#[tokio::test]
async fn test_replay_horizon_future_events() {
    // Create an event with a future timestamp (shouldn't happen but let's handle it)
    let future_timestamp = Timestamp::now() + Duration::from_secs(60 * 60); // 1 hour in future
    
    let event = EventBuilder::new(Kind::TextNote, "Future message")
        .custom_created_at(future_timestamp)
        .sign(&Keys::generate())
        .await
        .unwrap();
    
    // Future events should be accepted (they're not old)
    assert!(!event_handler::is_event_too_old(&event));
}