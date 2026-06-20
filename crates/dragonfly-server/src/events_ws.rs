//! WebSocket event stream for external consumers (web UI, orchestrators like
//! Jetpack) — the replacement for the deprecated SSE `GET /api/events`.
//!
//! Subscribes to the server's `EventManager` broadcast bus and forwards every
//! event as a JSON message, so consumers can track the fleet fully event-driven
//! with no polling. See issue #20.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use tokio::time::interval;
use tracing::warn;

use crate::AppState;

/// WebSocket ping interval for liveness detection.
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// HTTP handler that upgrades an event-stream connection to a WebSocket.
///
/// The WebSocket successor to the deprecated SSE `GET /api/events`. Open to any
/// caller (UI and external orchestrators); the stream is fan-out-only — clients
/// receive events, they don't need to send. On connect the server emits a single
/// `{"type":"stream_ready"}` message so clients can confirm the stream is live
/// (and gate on it) before acting.
pub async fn events_ws_handler(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| run_events_ws(socket, state))
}

/// Drive a single event-stream WebSocket for its lifetime.
async fn run_events_ws(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut event_rx = state.event_manager.subscribe();

    // Announce the stream is live (also lets clients/tests gate on subscription).
    let ready = serde_json::json!({ "type": "stream_ready" }).to_string();
    if sender.send(Message::Text(ready.into())).await.is_err() {
        return;
    }

    let mut ping = interval(PING_INTERVAL);
    ping.tick().await; // discard the immediate first tick

    loop {
        tokio::select! {
            evt = event_rx.recv() => match evt {
                Ok(event_string) => {
                    if let Some(envelope) = event_envelope(&event_string) {
                        let json = serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_string());
                        if sender.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(lag = n, "events websocket lagged; some events dropped");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            msg = receiver.next() => match msg {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(Message::Ping(p))) => { let _ = sender.send(Message::Pong(p)).await; }
                Some(Ok(_)) => {} // ignore inbound text/binary/pong
                Some(Err(_)) => break,
            },
            _ = ping.tick() => {
                if sender.send(Message::Ping(Bytes::new())).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = sender.send(Message::Close(None)).await;
}

/// Build the uniform JSON envelope for an event: `{"type", "payload"}`.
///
/// `payload` is the id string for ordinary events
/// (`machine_updated:<uuid>` -> `payload` = `"<uuid>"`) and the parsed JSON
/// object for raw-payload events (`workflow_progress:{...}`). Returns `None` for
/// raw-payload events that arrive without a payload (mirrors the SSE handler's
/// skip-on-missing-payload behaviour).
pub(crate) fn event_envelope(event_string: &str) -> Option<serde_json::Value> {
    let (kind, payload) = event_string.split_once(':').unwrap_or((event_string, ""));
    if matches!(kind, "ip_download_progress" | "workflow_progress" | "cluster") {
        if payload.is_empty() {
            return None;
        }
        let parsed: serde_json::Value =
            serde_json::from_str(payload).unwrap_or_else(|_| serde_json::json!(payload));
        Some(serde_json::json!({ "type": kind, "payload": parsed }))
    } else {
        Some(serde_json::json!({ "type": kind, "payload": payload }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_for_ordinary_event() {
        let v = event_envelope("machine_updated:abc-123").unwrap();
        assert_eq!(v["type"], "machine_updated");
        assert_eq!(v["payload"], "abc-123");
    }

    #[test]
    fn envelope_for_event_without_id() {
        let v = event_envelope("templates_ready").unwrap();
        assert_eq!(v["type"], "templates_ready");
        assert_eq!(v["payload"], "");
    }

    #[test]
    fn envelope_for_raw_payload_event() {
        let v = event_envelope("workflow_progress:{\"step\":3,\"done\":false}").unwrap();
        assert_eq!(v["type"], "workflow_progress");
        assert_eq!(v["payload"]["step"], 3);
        assert_eq!(v["payload"]["done"], false);
    }

    #[test]
    fn envelope_drops_raw_payload_event_without_payload() {
        assert!(event_envelope("workflow_progress").is_none());
    }

    /// End-to-end: a real client receives events forwarded from the EventManager.
    #[tokio::test]
    async fn events_ws_forwards_eventmanager_events() {
        use crate::api::api_router;
        use crate::test_helpers::create_test_app_state;
        use tokio_tungstenite::{connect_async, tungstenite::Message};

        let state = create_test_app_state().await;
        let app = axum::Router::new()
            .nest("/api", api_router())
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service()).await;
        });

        let url = format!("ws://{addr}/api/events/ws");
        let (mut ws, _) = connect_async(url.as_str()).await.unwrap();

        // The server announces readiness after subscribing, so by the time we've
        // read it, the handler is subscribed — no race when we then send.
        let ready = ws.next().await.unwrap().unwrap();
        let ready_str = match ready {
            Message::Text(t) => t.to_string(),
            other => panic!("expected stream_ready text, got {other:?}"),
        };
        let ready_v: serde_json::Value = serde_json::from_str(&ready_str).unwrap();
        assert_eq!(ready_v["type"], "stream_ready");

        // Push an event on the bus; the WS must forward it.
        let _ = state
            .event_manager
            .send("machine_updated:deadbeef".to_string());
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for forwarded event")
            .unwrap()
            .unwrap();
        let text = match msg {
            Message::Text(t) => t.to_string(),
            other => panic!("expected text, got {other:?}"),
        };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["type"], "machine_updated");
        assert_eq!(v["payload"], "deadbeef");
    }
}
