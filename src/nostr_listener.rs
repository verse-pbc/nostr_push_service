use crate::{
    error::{Result, ServiceError},
    state::AppState,
};
use nostr_sdk::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

pub async fn run(
    state: Arc<AppState>,
    event_tx: Sender<Box<Event>>,
    token: CancellationToken,
) -> Result<()> {
    info!("Starting Nostr listener...");

    let service_keys = state
        .service_keys
        .clone()
        .ok_or_else(|| ServiceError::Internal("Nostr service keys not configured".to_string()))?;
    let service_pubkey = service_keys.public_key();

    let client = Client::builder().signer(service_keys).build();

    let relay_url_str = &state.settings.nostr.relay_url;
    client.add_relay(relay_url_str.as_str()).await?;

    tracing::info!("Connecting to Nostr relays: {}", relay_url_str);
    client.connect().await;
    tracing::info!("Nostr client connected.");

    let process_window_duration =
        Duration::from_secs(state.settings.service.process_window_days as u64 * 24 * 60 * 60);
    let since_timestamp = Timestamp::now() - process_window_duration;

    let historical_kinds = state
        .settings
        .service
        .listen_kinds
        .iter()
        .filter_map(|&k| match u16::try_from(k) {
            Ok(kind_u16) => Some(Kind::from(kind_u16)),
            Err(_) => {
                tracing::warn!("Kind {} from config is too large, skipping.", k);
                None
            }
        })
        .collect::<Vec<_>>();

    let historical_filter = Filter::new()
        .kinds(historical_kinds.clone())
        .since(since_timestamp);

    info!(since = %since_timestamp, "Querying historical events...");
    let now = Timestamp::now();

    tokio::select! {
        biased;
        _ = token.cancelled() => {
             info!("Nostr listener cancelled before historical event query.");
             return Ok(());
        }
        fetch_result = client.fetch_events(historical_filter.clone(), Duration::from_secs(60)) => {
             match fetch_result {
                 Ok(historical_events) => {
                     info!(
                         count = historical_events.len(),
                         "Fetched historical events. Processing..."
                     );
                     for event in historical_events {
                         tokio::select! {
                             biased;
                             _ = token.cancelled() => {
                                 info!("Nostr listener cancelled during historical event processing.");
                                 return Ok(());
                             }
                             res = async {
                                 if event.pubkey == service_pubkey {
                                     return Ok(());
                                 }
                                 let event_id = event.id;
                                 tokio::select! {
                                     biased;
                                     _ = token.cancelled() => {
                                         info!("Nostr listener cancelled while sending historical event {}.", event_id);
                                         Err(ServiceError::Cancelled)
                                     }
                                     send_res = event_tx.send(Box::new(event)) => {
                                         if let Err(e) = send_res {
                                             error!(error = %e, event_id = %event_id, "Failed to send historical event to handler task");
                                             error!("Event handler channel likely closed, stopping historical processing.");
                                             Err(ServiceError::Internal(format!(
                                                 "Event handler channel closed during historical processing: {}", e
                                             )))
                                         } else {
                                            Ok(())
                                         }
                                     }
                                 }
                             } => {
                                  match res {
                                      Ok(_) => { /* Event processed or skipped successfully */ }
                                      Err(ServiceError::Cancelled) => return Ok(()),
                                      Err(e) => return Err(e),
                                  }
                             }
                         }
                     }
                     info!("Finished processing historical events.");
                 }
                 Err(e) => {
                     error!(error = %e, "Failed to query historical events");
                     warn!("Proceeding without historical events due to query failure.");
                     if token.is_cancelled() {
                         info!("Nostr listener cancelled after failed historical event query.");
                         return Ok(());
                     }
                 }
            }
        }
    }

    info!("Subscribing to live events...");
    let live_filter = Filter::new().kinds(historical_kinds).since(now);

    tokio::select! {
        biased;
        _ = token.cancelled() => {
             info!("Nostr listener cancelled before live event subscription.");
             return Ok(());
        }
        sub_result = client.subscribe(live_filter, None) => {
            if let Err(e) = sub_result {
                error!(error = %e, "Failed to subscribe to live events");
                return Err(e.into());
            }
        }
    }

    info!("Nostr listener subscribed and running.");

    let mut notifications = client.notifications();
    loop {
        tokio::select! {
            biased;
             _ = token.cancelled() => {
                 info!("Nostr listener cancellation received. Shutting down...");
                 break;
             }

            res = notifications.recv() => {
                 match res {
                    Ok(notification) => {
                        match notification {
                            nostr_sdk::RelayPoolNotification::Event { event, .. } => {
                                if event.pubkey == service_pubkey {
                                    continue;
                                }
                                let event_id = event.id;
                                let event_kind = event.kind;

                                debug!(event_id = %event_id, kind = %event_kind, "Received live event");

                                tokio::select! {
                                    biased;
                                    _ = token.cancelled() => {
                                        info!("Nostr listener cancelled while attempting to send live event {}.", event_id);
                                        break;
                                    }
                                    send_res = event_tx.send(event) => {
                                        if let Err(e) = send_res {
                                            tracing::error!(
                                                "Failed to send event {} to handler channel: {}",
                                                event_id,
                                                e
                                            );
                                            break;
                                        } else {
                                             debug!(event_id = %event_id, "Sent live event to handler");
                                        }
                                    }
                                }
                            }
                            nostr_sdk::RelayPoolNotification::Message { relay_url, message } => {
                                debug!(%relay_url, ?message, "Received message from relay");
                            }
                            nostr_sdk::RelayPoolNotification::Shutdown => {
                                tracing::info!("Nostr client shutdown notification received.");
                                break;
                            }
                            // Commented out potentially removed variants
                            /*
                            nostr_sdk::RelayPoolNotification::Stop { .. } => {
                                tracing::debug!("Received STOP notification");
                            }
                             nostr_sdk::RelayPoolNotification::RelayStatus { relay_url, status } => {
                                tracing::debug!(%relay_url, ?status, "Relay status update");
                             }
                             */
                        }
                    }
                    Err(e) => {
                        // Any error receiving from the notification channel is likely fatal
                        error!("Error receiving from notification channel: {}. Shutting down listener.", e);
                        break; // Exit the loop on any receive error
                        /* Removed specific RecvError check
                        if matches!(e, tokio::sync::mpsc::error::RecvError) {
                             error!("Notification channel closed unexpectedly.");
                             break;
                         }
                        */
                    }
                }
            }
        }
    }

    info!("Nostr listener shutting down.");
    client.disconnect().await;
    info!("Nostr client disconnected.");
    Ok(())
}
