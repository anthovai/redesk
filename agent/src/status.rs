//! Live agent state shared with the desktop UI.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use tokio::sync::{Notify, mpsc, oneshot};

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
    /// The session is being recorded (policy from the server); shown as "กำลังบันทึก".
    pub recording: bool,
    /// Chat, voice call and switch sides with this viewer (connection manager).
    pub link: Arc<SessionLink>,
}

/// One chat message of a session (kept in memory only, gone when the session ends).
#[derive(Clone, Debug)]
pub struct ChatLine {
    /// Written on this machine (false = from the viewer).
    pub mine: bool,
    pub text: String,
    pub at: SystemTime,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CallState {
    Idle,
    /// The viewer is calling; the person here has to accept.
    Ringing(Instant),
    Active { since: Instant, muted: bool, remote_muted: bool },
}

/// What the person at this machine did in the connection manager, for the session task.
#[derive(Debug)]
pub enum HostCmd {
    Chat(String),
    /// Accept or decline the ringing call.
    Answer(bool),
    Hangup,
    Mute(bool),
    /// Switch sides: declined, or the switched session failed to start (with the reason).
    SwitchFailed(String),
}

/// Live in-session communication with one viewer, shared by the session task and the UI.
pub struct SessionLink {
    /// What the viewer supports (from `auth.features`) and this machine allows.
    pub chat: bool,
    pub voice: bool,
    pub switch: bool,
    pub lines: Mutex<Vec<ChatLine>>,
    /// Viewer messages not seen yet in the connection manager.
    pub unread: AtomicUsize,
    pub call: Mutex<CallState>,
    /// A switch-sides invitation waiting for an answer: (server grant, viewer secret, received).
    pub switch_offer: Mutex<Option<(String, String, Instant)>>,
    pub cmd: mpsc::UnboundedSender<HostCmd>,
}

impl SessionLink {
    pub fn new(chat: bool, voice: bool, switch: bool) -> (Arc<Self>, mpsc::UnboundedReceiver<HostCmd>) {
        let (cmd, rx) = mpsc::unbounded_channel();
        let link = Self {
            chat,
            voice,
            switch,
            lines: Mutex::default(),
            unread: AtomicUsize::new(0),
            call: Mutex::new(CallState::Idle),
            switch_offer: Mutex::default(),
            cmd,
        };
        (Arc::new(link), rx)
    }

    /// A link with nothing negotiated (demo sessions).
    pub fn none() -> Arc<Self> {
        Self::new(false, false, false).0
    }

    pub fn push(&self, mine: bool, text: String) {
        self.lines.lock().unwrap().push(ChatLine { mine, text, at: SystemTime::now() });
        if !mine {
            self.unread.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn send_chat(&self, text: &str) {
        let text: String = text.trim().chars().take(crate::proto::CHAT_MAX).collect();
        if !text.is_empty() {
            let _ = self.cmd.send(HostCmd::Chat(text));
        }
    }

    pub fn call_state(&self) -> CallState {
        *self.call.lock().unwrap()
    }

    pub fn unread(&self) -> usize {
        self.unread.load(Ordering::Relaxed)
    }

    pub fn mark_read(&self) {
        self.unread.store(0, Ordering::Relaxed);
    }
}

/// Someone wants to take control; the UI answers through `reply`.
pub struct ApprovalRequest {
    pub sid: String,
    pub who: String,
    pub ip: String,
    /// The session will be recorded if accepted (shown to the person deciding).
    pub recording: bool,
    pub reply: oneshot::Sender<bool>,
}

pub struct Status {
    pub server: ServerState,
    pub sessions: Vec<SessionInfo>,
    /// Annotations viewers draw on this screen, one board per session that allows it.
    pub boards: Vec<crate::whiteboard::Board>,
    /// Notifications not yet shown by the UI.
    pub attention: Vec<Attention>,
}

impl Default for Status {
    fn default() -> Self {
        Self { server: ServerState::Connecting, sessions: Vec::new(), boards: Vec::new(), attention: Vec::new() }
    }
}

/// Hooks installed by the desktop UI.
pub struct UiHooks {
    pub approvals: tokio::sync::mpsc::UnboundedSender<ApprovalRequest>,
    pub repaint: Arc<dyn Fn() + Send + Sync>,
}

/// Something happened in a session that the person here should notice (chat message, call).
#[derive(Clone, Debug)]
pub struct Attention {
    pub sid: String,
    pub who: String,
    pub text: String,
}

impl Status {
    /// Queues a notification for the UI (connection manager focus, system notification).
    pub fn notify(&mut self, a: Attention) {
        self.attention.push(a);
    }
}
