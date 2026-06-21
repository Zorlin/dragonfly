//! Reusable verifier for the `/api/events/ws` stream (orchestrator side).
//!
//! Connects, reads the `stream_ready` hello (which proves it's subscribed), then
//! triggers a checkin — which emits `machine_updated` on the bus — and confirms
//! the event is forwarded live over the WebSocket. Cleans up the throwaway
//! machine it registers. A reference for event-driven consumers (e.g. Jetpack).
//!
//! Usage: events_probe [WS_BASE_URL]    # default ws://localhost:3000

use std::time::Duration;

use anyhow::Context;
use futures_util::StreamExt;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const MAC: &str = "00:11:22:33:44:bb";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://localhost:3000".to_string());
    let http = base
        .replacen("ws://", "http://", 1)
        .replacen("wss://", "https://", 1);

    let url = format!("{base}/api/events/ws");
    println!("connecting to {url}");
    let (mut ws, _) = connect_async(url.as_str()).await?;
    let hello = ws.next().await.unwrap().unwrap();
    println!("hello <- {hello}"); // {"type":"stream_ready"}

    // Subscribed now (hello is sent after subscribe): trigger a checkin, which
    // emits machine_updated on the bus.
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{http}/api/agent/checkin"))
        .json(&serde_json::json!({ "mac": MAC, "all_macs": [MAC], "existing_os": null }))
        .send()
        .await?;
    let body: serde_json::Value = serde_json::from_str(&resp.text().await?)?;
    let machine_id = body["machine_id"].as_str().unwrap_or("").to_string();
    println!("checkin -> machine_id={machine_id}");

    // The events WS must forward the machine_updated event.
    let mut saw = false;
    for _ in 0..10 {
        let msg = match timeout(Duration::from_secs(3), ws.next()).await {
            Ok(Some(Ok(m))) => m,
            _ => break,
        };
        if let Message::Text(t) = &msg {
            println!("event <- {t}");
            if t.contains("machine_updated") {
                saw = true;
                break;
            }
        }
    }

    // Clean up the throwaway machine.
    let _ = client
        .post(format!("{http}/api/agent/remove"))
        .json(&serde_json::json!({ "machine_id": machine_id, "mac": MAC }))
        .send()
        .await;

    if saw {
        println!("PASS: /api/events/ws forwarded machine_updated live");
        Ok(())
    } else {
        anyhow::bail!("did not receive machine_updated on the events WS")
    }
}
