use tokio::sync::broadcast;
use tracing::{info, warn};

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

    // Publish an event, returning Result to handle errors
    pub fn send(&self, message: String) -> Result<usize, broadcast::error::SendError<String>> {
        let receivers = self.tx.receiver_count();
        
        // Only attempt to send if we have receivers to avoid log spam
        if receivers > 0 {
            match self.tx.send(message.clone()) {
                Ok(n) => {
                    info!("Event sent to {} receivers: {}", n, message);
                    Ok(n)
                },
                Err(e) => {
                    warn!("Failed to send event: {}", e);
                    Err(e)
                }
            }
        } else {
            // Create a more descriptive error when there are no receivers
            warn!("No receivers for event: {}", message);
            Err(broadcast::error::SendError(message))
        }
    }
    
    // Get the current receiver count
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
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