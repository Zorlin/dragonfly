//! WebSocket push channel — agent side.
//!
//! When the server URL is `ws://`/`wss://`, Mage replaces its 30s checkin poll
//! with this persistent connection. The agent sends a `HardwareCheckIn` as the
//! first message, then waits for the server to push `CheckInResponse`
//! instructions — so a reimage or OS assignment is picked up near-instantly
//! instead of after the next poll. Steady state is one open socket with nothing
//! sent on a timer; exponential backoff with small jitter applies only to
//! reconnection after a drop.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use dragonfly_crd::Hardware;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

use crate::probe::DetectedOs;
use crate::workflow::{AgentAction, AgentHardwareInfo, CheckInResponse, build_checkin_payload};

/// Minimum and maximum reconnect backoff.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Run the WebSocket provisioning loop: connect, drive, reconnect on drop.
///
/// Returns once the server has directed a terminal action (Execute/Reboot/
/// LocalBoot) that `handle_agent_action` has carried out.
///
/// `ws_base_url` is the WebSocket base (e.g. `ws://host:3000`); `http_base_url`
/// is the HTTP base used to fetch the workflow when an `Execute` arrives.
pub async fn run_ws_provisioning_loop(
    client: &Client,
    ws_base_url: &str,
    http_base_url: &str,
    mac: &str,
    hostname: Option<&str>,
    ip_address: Option<&str>,
    existing_os: &Option<DetectedOs>,
    hardware: &Hardware,
    agent_hw_info: &AgentHardwareInfo,
    action_filter: &Option<Vec<usize>>,
) -> Result<()> {
    let endpoint = format!("{}/api/agent/ws", ws_base_url);
    info!(endpoint = %endpoint, "Starting WebSocket provisioning loop");

    let mut backoff = BACKOFF_MIN;
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let outcome = drive_connection(
            &endpoint,
            client,
            http_base_url,
            mac,
            hostname,
            ip_address,
            existing_os,
            hardware,
            agent_hw_info,
            action_filter,
        )
        .await;

        match outcome {
            Ok(true) => return Ok(()), // terminal action handled
            Ok(false) => {
                // Clean disconnect — reconnect promptly.
                backoff = BACKOFF_MIN;
            }
            Err(e) => {
                warn!(error = %e, attempt, "WebSocket connection lost, will reconnect");
            }
        }

        // Reconnect backoff + ±3ms jitter. Steady state never reaches here: the
        // socket stays open and the agent sends nothing on a timer.
        sleep(backoff_with_jitter(backoff)).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Drive a single connection. Returns `Ok(true)` when a terminal action was
/// handled, `Ok(false)` on a clean disconnect, `Err` on a connection error.
async fn drive_connection(
    endpoint: &str,
    client: &Client,
    http_base_url: &str,
    mac: &str,
    hostname: Option<&str>,
    ip_address: Option<&str>,
    existing_os: &Option<DetectedOs>,
    hardware: &Hardware,
    agent_hw_info: &AgentHardwareInfo,
    action_filter: &Option<Vec<usize>>,
) -> Result<bool> {
    let (ws_stream, _response) = connect_async(endpoint)
        .await
        .context("WebSocket connect failed")?;
    let (mut write, mut read) = ws_stream.split();
    info!("WebSocket connected");

    // First message identifies this machine — identical payload to an HTTP checkin.
    let payload = build_checkin_payload(
        mac,
        hostname,
        ip_address,
        existing_os.as_ref(),
        Some(agent_hw_info),
    );
    let json = serde_json::to_string(&payload).context("serialize checkin payload")?;
    write
        .send(Message::Text(json.into()))
        .await
        .context("send checkin over WebSocket")?;

    while let Some(msg) = read.next().await {
        let msg = msg.context("WebSocket receive")?;
        match msg {
            Message::Text(text) => {
                let resp: CheckInResponse = match serde_json::from_str(text.as_str()) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(error = %e, "ignoring malformed WebSocket message");
                        continue;
                    }
                };
                info!(action = ?resp.action, "WebSocket checkin response");
                crate::handle_agent_action(
                    &resp,
                    existing_os,
                    client,
                    http_base_url,
                    hardware,
                    action_filter,
                )
                .await?;
                if resp.action != AgentAction::Wait {
                    // Execute/Reboot/LocalBoot carried out (the process typically
                    // reboots/exits as part of handling it).
                    return Ok(true);
                }
            }
            Message::Close(_) => return Ok(false),
            // Ping/Pong are answered automatically by tungstenite; Binary is ignored.
            _ => {}
        }
    }

    // Stream ended without an explicit close frame.
    Ok(false)
}

/// Add ±3ms jitter to a backoff duration, derived from the clock so we avoid
/// pulling in a random-number dependency just to de-sync reconnects.
fn backoff_with_jitter(backoff: Duration) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    // subsec_nanos % 7 -> [0, 6]; shift to [-3, +3] ms.
    let jitter_ms = (nanos % 7) as i64 - 3;
    let total_ms = (backoff.as_millis() as i64 + jitter_ms).max(0) as u64;
    Duration::from_millis(total_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_within_plus_minus_3ms_of_backoff() {
        for _ in 0..1000 {
            let jittered = backoff_with_jitter(Duration::from_secs(5));
            let delta = jittered.as_millis() as i64 - 5_000;
            assert!(
                (-3..=3).contains(&delta),
                "jittered backoff {jittered:?} deviates {delta}ms from 5s"
            );
        }
    }

    #[test]
    fn jitter_never_goes_negative_for_tiny_backoff() {
        // 0ms backoff + up to -3ms jitter must clamp to 0, not underflow.
        let jittered = backoff_with_jitter(Duration::from_millis(0));
        assert!(jittered.as_millis() <= 3);
    }
}
