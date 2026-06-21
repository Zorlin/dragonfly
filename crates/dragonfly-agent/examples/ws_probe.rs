//! Reusable debug client for the agent WebSocket push channel.
//!
//! Connects to a Dragonfly `/api/agent/ws`, sends a `HardwareCheckIn`, prints
//! the initial response, then triggers a reimage via `/api/agent/request-install`
//! (MAC-authed, no login) and prints the pushed response — proving the push path
//! against a real server. It never executes a workflow; it only observes.
//!
//! Usage:
//!     ws_probe [WS_BASE_URL]            # default ws://localhost:3000
//!
//! Env:
//!     WS_PROBE_MAC        MAC address to register as (default 00:11:22:33:44:aa)
//!     WS_PROBE_TEMPLATE   OS template to request-install (default debian-12)
//!
//! Example:
//!     WS_PROBE_TEMPLATE=debian-13 cargo run --release --example ws_probe -- ws://10.7.1.100:3000

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://localhost:3000".to_string());
    let mac = env_or("WS_PROBE_MAC", "00:11:22:33:44:aa");
    let template = env_or("WS_PROBE_TEMPLATE", "debian-12");
    let http_base = base
        .replacen("ws://", "http://", 1)
        .replacen("wss://", "https://", 1);

    let url = format!("{base}/api/agent/ws");
    println!("connecting to {url} (mac={mac}, template={template})");
    let (mut ws, _) = connect_async(url.as_str()).await?;
    println!("connected");

    let checkin = serde_json::json!({ "mac": mac, "all_macs": [mac], "existing_os": null });
    ws.send(Message::Text(checkin.to_string().into())).await?;
    println!("sent checkin");

    let first = timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("timed out waiting for initial response")
        .unwrap()
        .unwrap();
    let first_str = match first {
        Message::Text(t) => t.to_string(),
        other => panic!("expected text, got {other:?}"),
    };
    let initial: serde_json::Value = serde_json::from_str(&first_str).unwrap();
    let action = initial["action"].as_str().unwrap_or("?");
    let machine_id = initial["machine_id"].as_str().unwrap_or("").to_string();
    println!("initial  <- action={action} machine_id={machine_id}");
    assert_eq!(action, "wait", "fresh no-OS machine should get wait");

    // Trigger a reimage via the MAC-authed agent endpoint (emits machine_updated).
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{http_base}/api/agent/request-install"))
        .json(&serde_json::json!({
            "machine_id": machine_id,
            "mac": mac,
            "template_name": template,
        }))
        .send()
        .await?;
    println!("request-install ({template}) -> HTTP {}", resp.status());

    let pushed = timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("timed out waiting for push")
        .unwrap()
        .unwrap();
    let pushed_str = match pushed {
        Message::Text(t) => t.to_string(),
        other => panic!("expected text push, got {other:?}"),
    };
    let pushed: serde_json::Value = serde_json::from_str(&pushed_str).unwrap();
    println!(
        "pushed   <- action={} workflow_id={}",
        pushed["action"].as_str().unwrap_or("?"),
        pushed["workflow_id"]
    );
    assert_eq!(pushed["action"].as_str(), Some("execute"));
    println!("PASS: server pushed execute within the window");

    Ok(())
}
