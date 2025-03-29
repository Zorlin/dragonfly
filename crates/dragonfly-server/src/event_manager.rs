use tokio::sync::broadcast;
use tracing::info;

// Event types that can be published
#[derive(Debug, Clone)]
pub enum Event {
    MachineDiscovered(String),
    MachineUpdated(String),
    MachineDeleted(String),
}

// Event manager for publishing SSE events
pub struct EventManager {
    tx: broadcast::Sender<String>,
}

impl EventManager {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(100);
        Self { tx }
    }

    // Create a new subscription to events
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    // Publish an event
    pub fn send(&self, message: String) {
        if let Err(e) = self.tx.send(message.clone()) {
            info!("Failed to send event: {}", e);
        }
    }
}

impl Default for EventManager {
    fn default() -> Self {
        Self::new()
    }
}

// Make EventManager safe to clone by wrapping in Arc
impl Clone for EventManager {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
} 