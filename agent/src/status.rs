//! Live agent state shared with the desktop UI.

use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::{Notify, oneshot};

#[derive(Clone, Debug, PartialEq)]
pub enum ServerState {
    Connecting,
    Online,
    Error(String),
}

/// A remote-control session that passed authentication.
#[derive(Clone)]
pub struct SessionInfo {
    pub sid: String,
    pub email: String,
    pub name: String,
    pub ip: String,
    pub started: SystemTime,
    /// Notified to end the session from the UI.
    pub kill: Arc<Notify>,
}

/// Someone wants to take control; the UI answers through `reply`.
pub struct ApprovalRequest {
    pub sid: String,
    pub who: String,
    pub ip: String,
    pub reply: oneshot::Sender<bool>,
}

pub struct Status {
    pub server: ServerState,
    pub sessions: Vec<SessionInfo>,
}

impl Default for Status {
    fn default() -> Self {
        Self { server: ServerState::Connecting, sessions: Vec::new() }
    }
}

/// Hooks installed by the desktop UI.
pub struct UiHooks {
    pub approvals: tokio::sync::mpsc::UnboundedSender<ApprovalRequest>,
    pub repaint: Arc<dyn Fn() + Send + Sync>,
}
