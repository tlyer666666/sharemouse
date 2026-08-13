use crate::config::Edge;
use crate::protocol::WireMessage;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc::SyncSender, mpsc::TrySendError, Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod stub;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
use macos as implementation;
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
use stub as implementation;
#[cfg(target_os = "windows")]
use windows as implementation;

#[derive(Debug)]
pub enum CaptureEvent {
    EdgeReached(Edge),
    Input(WireMessage),
    EmergencyRelease,
    PeerReturned { session_id: u64 },
    PeerDisconnected { session_id: u64, error: String },
    PeerPong { session_id: u64, stamp: u64 },
}

pub struct CaptureControl {
    pub enabled: AtomicBool,
    pub active: AtomicBool,
    pub pending: AtomicBool,
    pub overflowed: AtomicBool,
    pub transition: Mutex<()>,
    pub edge: AtomicU8,
    pub edge_delay_ms: AtomicU64,
}

impl CaptureControl {
    pub fn new(enabled: bool, edge: Edge, edge_delay_ms: u64) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            active: AtomicBool::new(false),
            pending: AtomicBool::new(false),
            overflowed: AtomicBool::new(false),
            transition: Mutex::new(()),
            edge: AtomicU8::new(edge.as_u8()),
            edge_delay_ms: AtomicU64::new(edge_delay_ms),
        }
    }

    pub fn update(&self, enabled: bool, edge: Edge, edge_delay_ms: u64) {
        let _transition = self.transition.lock().ok();
        self.enabled.store(enabled, Ordering::Release);
        self.edge.store(edge.as_u8(), Ordering::Release);
        self.edge_delay_ms.store(edge_delay_ms, Ordering::Release);
        if !enabled {
            self.active.store(false, Ordering::Release);
            self.pending.store(false, Ordering::Release);
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionState {
    pub capture: bool,
    pub injection: bool,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionOutcome {
    Continue,
    ReturnRequested,
}

pub struct Injector {
    pressed_keys: HashSet<u16>,
    pressed_buttons: HashSet<u8>,
    return_edge: Edge,
    activated_at: Instant,
}

type ReleaseRequest = (HashSet<u16>, HashSet<u8>);
static RELEASE_WORKER: OnceLock<SyncSender<ReleaseRequest>> = OnceLock::new();

impl Injector {
    pub fn new() -> Self {
        Self {
            pressed_keys: HashSet::new(),
            pressed_buttons: HashSet::new(),
            return_edge: Edge::Disabled,
            activated_at: Instant::now(),
        }
    }

    pub fn activate(&mut self, entry_edge: Edge) -> Result<(), String> {
        self.release_all();
        self.return_edge = entry_edge;
        self.activated_at = Instant::now();
        implementation::place_pointer(entry_edge)
    }

    pub fn apply(&mut self, message: &WireMessage) -> Result<InjectionOutcome, String> {
        match message {
            WireMessage::MouseMove { dx, dy } => {
                implementation::inject_mouse_move(*dx, *dy)?;
                if self.activated_at.elapsed() > Duration::from_millis(350)
                    && self.pressed_buttons.is_empty()
                    && moving_outward(self.return_edge, *dx)
                    && implementation::pointer_at_edge(self.return_edge)?
                {
                    return Ok(InjectionOutcome::ReturnRequested);
                }
            }
            WireMessage::MouseButton { button, down } => {
                implementation::inject_mouse_button(*button, *down)?;
                if *down {
                    self.pressed_buttons.insert(*button);
                } else {
                    self.pressed_buttons.remove(button);
                }
            }
            WireMessage::Wheel {
                horizontal,
                vertical,
            } => implementation::inject_wheel(*horizontal, *vertical)?,
            WireMessage::Key { usage, down } => {
                implementation::inject_key(*usage, *down)?;
                if *down {
                    self.pressed_keys.insert(*usage);
                } else {
                    self.pressed_keys.remove(usage);
                }
            }
            WireMessage::ReleaseAll => self.release_all(),
            _ => {}
        }
        Ok(InjectionOutcome::Continue)
    }

    pub fn release_all(&mut self) {
        for attempt in 0..3 {
            self.pressed_keys
                .retain(|usage| implementation::inject_key(*usage, false).is_err());
            self.pressed_buttons
                .retain(|button| implementation::inject_mouse_button(*button, false).is_err());
            if self.pressed_keys.is_empty() && self.pressed_buttons.is_empty() {
                break;
            }
            if attempt < 2 {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        self.return_edge = Edge::Disabled;
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        self.release_all();
        if self.pressed_keys.is_empty() && self.pressed_buttons.is_empty() {
            return;
        }
        let request = (
            std::mem::take(&mut self.pressed_keys),
            std::mem::take(&mut self.pressed_buttons),
        );
        if let Some(worker) = RELEASE_WORKER.get() {
            if let Err(error) = worker.send(request) {
                retry_releases(error.0);
            }
        } else {
            retry_releases(request);
        }
    }
}

pub fn spawn_capture(
    sender: SyncSender<CaptureEvent>,
    control: Arc<CaptureControl>,
) -> Result<JoinHandle<()>, String> {
    ensure_release_worker()?;
    implementation::spawn_capture(sender, control)
}

fn ensure_release_worker() -> Result<(), String> {
    if RELEASE_WORKER.get().is_some() {
        return Ok(());
    }
    let (sender, receiver) = std::sync::mpsc::sync_channel(4);
    thread::Builder::new()
        .name("deskbridge-release-worker".into())
        .spawn(move || {
            while let Ok(request) = receiver.recv() {
                retry_releases(request);
            }
        })
        .map_err(|error| format!("无法启动输入释放清理器：{error}"))?;
    let _ = RELEASE_WORKER.set(sender);
    Ok(())
}

fn retry_releases((mut keys, mut buttons): ReleaseRequest) {
    let mut delay = Duration::from_millis(100);
    while !keys.is_empty() || !buttons.is_empty() {
        keys.retain(|usage| implementation::inject_key(*usage, false).is_err());
        buttons.retain(|button| implementation::inject_mouse_button(*button, false).is_err());
        if keys.is_empty() && buttons.is_empty() {
            break;
        }
        thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

fn try_send_capture(
    sender: &SyncSender<CaptureEvent>,
    control: &CaptureControl,
    event: CaptureEvent,
) {
    match sender.try_send(event) {
        Ok(()) => {}
        Err(TrySendError::Full(event)) => match event {
            CaptureEvent::Input(WireMessage::MouseMove { .. }) => {}
            CaptureEvent::EdgeReached(_) => {
                control.pending.store(false, Ordering::Release);
            }
            _ => {
                control.active.store(false, Ordering::Release);
                control.pending.store(false, Ordering::Release);
                control.overflowed.store(true, Ordering::Release);
            }
        },
        Err(TrySendError::Disconnected(_)) => {
            control.active.store(false, Ordering::Release);
            control.pending.store(false, Ordering::Release);
        }
    }
}

pub fn restore_local_pointer(exit_edge: Edge) -> Result<(), String> {
    implementation::restore_local_pointer(exit_edge)
}

pub fn permission_state() -> PermissionState {
    implementation::permission_state()
}

pub fn request_permissions() -> PermissionState {
    implementation::request_permissions()
}

fn moving_outward(edge: Edge, dx: i32) -> bool {
    match edge {
        Edge::Left => dx < 0,
        Edge::Right => dx > 0,
        Edge::Disabled => false,
    }
}
