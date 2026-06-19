//! WebSocket push channel for agents.
//!
//! A long-lived alternative to Mage's 30s checkin poll. The agent opens
//! `GET /api/agent/ws`, sends a `HardwareCheckIn` as the first message, and the
//! server replies with a `CheckInResponse`. The socket then stays open: whenever
//! the machine's intent changes (reimage requested, OS assigned, workflow
//! created), the server pushes a fresh `CheckInResponse` so the agent acts
//! near-instantly instead of waiting for the next poll.
//!
//! Intent is recomputed via `ProvisioningService::current_intent` on each
//! `machine_updated:{id}` notification (pull-on-notification, not event
//! sourcing), so steady state is one open socket with no periodic polling.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::time::{interval, timeout};
use tracing::{info, warn};
use uuid::Uuid;

use crate::provisioning::{AgentAction, CheckInResponse, HardwareCheckIn};
use crate::AppState;

/// Time to wait for the agent's first `HardwareCheckIn` message before giving up.
const FIRST_MESSAGE_TIMEOUT: Duration = Duration::from_secs(10);
/// WebSocket ping interval for liveness detection.
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// HTTP handler that upgrades an agent connection to a WebSocket.
///
/// Auth-less, matching `/api/agent/checkin`: agents have no tokens, and identity
/// is established MAC-bound from the first message via `handle_checkin`. A WS
/// connection is longer-lived than a POST, but the threat model is unchanged —
/// an attacker who can reach the server can already spoof checkins today.
pub async fn agent_ws_handler(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Response {
    if state.provisioning.is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Provisioning service not available"})),
        )
            .into_response();
    }
    ws.on_upgrade(move |socket| run_agent_ws(socket, state))
}

/// Drive a single agent WebSocket connection for its lifetime.
async fn run_agent_ws(socket: WebSocket, state: AppState) {
    let provisioning = match state.provisioning.clone() {
        Some(p) => p,
        None => return,
    };
    let (mut sender, mut receiver) = socket.split();

    // 1. First message must be a HardwareCheckIn.
    let checkin = match read_first_checkin(&mut receiver).await {
        Some(c) => c,
        None => {
            let _ = sender.send(Message::Close(None)).await;
            return;
        }
    };

    // 2. Initial checkin + response. handle_checkin does not emit (the HTTP
    //    handler does), so emit here to refresh the UI.
    let response = match provisioning.handle_checkin(&checkin).await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, mac = %checkin.mac, "agent ws checkin failed");
            let _ = sender.send(Message::Close(None)).await;
            return;
        }
    };
    info!(
        machine_id = %response.machine_id,
        action = ?response.action,
        "agent ws connected"
    );
    let machine_id: Uuid = match response.machine_id.parse() {
        Ok(id) => id,
        Err(_) => {
            let _ = sender.send(Message::Close(None)).await;
            return;
        }
    };
    let _ = state
        .event_manager
        .send(format!("machine_updated:{}", response.machine_id));
    if send_response(&mut sender, &response).await.is_err() {
        return;
    }
    let mut last_pushed: Option<(AgentAction, Option<String>)> =
        Some((response.action.clone(), response.workflow_id.clone()));

    // 3. Event loop over inbound messages, server-side notifications, and pings.
    //    Single task => `last_pushed` has one owner and sends are not concurrent.
    let mut event_rx = state.event_manager.subscribe();
    let mut ping = interval(PING_INTERVAL);
    ping.tick().await; // discard the immediate first tick

    loop {
        tokio::select! {
            msg = receiver.next() => match msg {
                Some(Ok(Message::Text(text))) => {
                    // Inbound text is an optional re-checkin (heartbeat refresh).
                    if let Ok(checkin) = serde_json::from_str::<HardwareCheckIn>(&text) {
                        if let Ok(resp) = provisioning.handle_checkin(&checkin).await {
                            let _ = state.event_manager.send(
                                format!("machine_updated:{}", resp.machine_id));
                            if should_push(last_pushed.as_ref(), &resp) {
                                if send_response(&mut sender, &resp).await.is_err() {
                                    break;
                                }
                                last_pushed =
                                    Some((resp.action.clone(), resp.workflow_id.clone()));
                            }
                        }
                    }
                }
                Some(Ok(Message::Ping(p))) => {
                    let _ = sender.send(Message::Pong(p)).await;
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {} // Binary, Pong, etc. — ignore
            },
            evt = event_rx.recv() => match evt {
                Ok(event) if event_concerns_machine(&event, machine_id) => {
                    match provisioning.current_intent(machine_id).await {
                        Ok(Some(intent)) => {
                            if should_push(last_pushed.as_ref(), &intent) {
                                if send_response(&mut sender, &intent).await.is_err() {
                                    break;
                                }
                                last_pushed =
                                    Some((intent.action.clone(), intent.workflow_id.clone()));
                            }
                        }
                        Ok(None) => {
                            // Machine vanished — close the socket.
                            let _ = sender.send(Message::Close(None)).await;
                            break;
                        }
                        Err(e) => warn!(error = %e, "current_intent failed in ws watcher"),
                    }
                }
                Ok(_) => {} // event for another machine or type
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // We may have missed the triggering event; recompute unconditionally.
                    warn!(lag = n, "ws event watcher lagged, recomputing intent");
                    if let Ok(Some(intent)) = provisioning.current_intent(machine_id).await {
                        if should_push(last_pushed.as_ref(), &intent) {
                            if send_response(&mut sender, &intent).await.is_err() {
                                break;
                            }
                            last_pushed =
                                Some((intent.action.clone(), intent.workflow_id.clone()));
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
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

/// Read and parse the first message as a `HardwareCheckIn`, with a timeout.
async fn read_first_checkin(receiver: &mut SplitStream<WebSocket>) -> Option<HardwareCheckIn> {
    let msg = timeout(FIRST_MESSAGE_TIMEOUT, receiver.next()).await.ok()??;
    match msg {
        Ok(Message::Text(text)) => serde_json::from_str(&text).ok(),
        _ => None,
    }
}

/// Serialize and send a `CheckInResponse` as a text message.
async fn send_response(
    sender: &mut SplitSink<WebSocket, Message>,
    response: &CheckInResponse,
) -> Result<(), ()> {
    let json = serde_json::to_string(response).map_err(|_| ())?;
    sender.send(Message::Text(json.into())).await.map_err(|_| ())
}

/// Whether a freshly-computed intent should be pushed to the agent.
///
/// The push channel only drives `Wait → Execute`; `LocalBoot`/`Reboot` are never
/// pushed (the agent already acted on its initial response, and the channel
/// cannot re-probe the disk). Returns true only when the action/workflow differs
/// from what was last pushed.
fn should_push(last: Option<&(AgentAction, Option<String>)>, new: &CheckInResponse) -> bool {
    if matches!(new.action, AgentAction::Reboot | AgentAction::LocalBoot) {
        return false;
    }
    let key = (new.action.clone(), new.workflow_id.clone());
    Some(&key) != last
}

/// Whether a server event string pertains to a given machine.
///
/// Events are `"{type}:{uuid}"`. We react to `machine_updated`/`machine_deleted`
/// for our machine only.
fn event_concerns_machine(event: &str, machine_id: Uuid) -> bool {
    let Some((kind, id)) = event.split_once(':') else {
        return false;
    };
    let kind_match = matches!(kind, "machine_updated" | "machine_deleted");
    let id_match = Uuid::parse_str(id.trim()).map_or(false, |parsed| parsed == machine_id);
    kind_match && id_match
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(action: AgentAction, wf: Option<&str>) -> CheckInResponse {
        CheckInResponse {
            machine_id: Uuid::now_v7().to_string(),
            memorable_name: "m".to_string(),
            is_new: false,
            action,
            workflow_id: wf.map(str::to_string),
        }
    }

    #[test]
    fn event_concerns_machine_matches_our_machine() {
        let id = Uuid::now_v7();
        assert!(event_concerns_machine(&format!("machine_updated:{id}"), id));
        assert!(event_concerns_machine(&format!("machine_deleted:{id}"), id));
    }

    #[test]
    fn event_concerns_machine_ignores_other_machines_and_types() {
        let id = Uuid::now_v7();
        let other = Uuid::now_v7();
        assert!(!event_concerns_machine(&format!("machine_updated:{other}"), id));
        assert!(!event_concerns_machine(&format!("workflow_progress:{id}"), id));
        assert!(!event_concerns_machine("templates_ready", id));
    }

    #[test]
    fn should_push_only_wait_or_execute_and_only_on_change() {
        let wait = resp(AgentAction::Wait, None);
        let exec1 = resp(AgentAction::Execute, Some("wf-1"));
        let exec2 = resp(AgentAction::Execute, Some("wf-2"));
        let localboot = resp(AgentAction::LocalBoot, None);
        let reboot = resp(AgentAction::Reboot, None);

        // Nothing pushed yet: Wait and Execute push; LocalBoot/Reboot never do.
        assert!(should_push(None, &wait));
        assert!(should_push(None, &exec1));
        assert!(!should_push(None, &localboot));
        assert!(!should_push(None, &reboot));

        // After Execute(wf-1) was pushed, the same doesn't re-push; a different
        // workflow does, and a return to Wait also does.
        let last = (AgentAction::Execute, Some("wf-1".to_string()));
        assert!(!should_push(Some(&last), &exec1));
        assert!(should_push(Some(&last), &exec2));
        assert!(should_push(Some(&last), &wait));
        assert!(!should_push(Some(&last), &localboot));
    }
}
