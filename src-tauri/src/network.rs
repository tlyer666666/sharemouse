use crate::config::{Edge, Settings};
use crate::discovery::{self, PeerInfo, PeerTable};
use crate::platform::{self, CaptureControl, CaptureEvent, InjectionOutcome, Injector};
use crate::protocol::{self, SecureWriter, WireMessage};
use serde::Serialize;
use std::collections::HashMap;
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const SESSION_IDLE: u8 = 0;
const SESSION_OUTGOING: u8 = 1;
const SESSION_INCOMING: u8 = 2;
const MOUSE_MOVE_INTERVAL: Duration = Duration::from_millis(8);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(4);
const INPUT_QUEUE_CAPACITY: usize = 512;
const MAX_PREAUTH_CONNECTIONS: usize = 4;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub state: String,
    pub message: String,
    pub active_peer: Option<String>,
    pub latency_ms: Option<u64>,
    pub listening: bool,
    pub last_error: Option<String>,
}

impl RuntimeStatus {
    fn paused() -> Self {
        Self {
            state: "paused".into(),
            message: "共享已暂停，本机输入不会发送。".into(),
            active_peer: None,
            latency_ms: None,
            listening: false,
            last_error: None,
        }
    }
}

pub struct Service {
    pub settings: Arc<RwLock<Settings>>,
    pub peers: PeerTable,
    pub status: Arc<RwLock<RuntimeStatus>>,
    pub capture_control: Arc<CaptureControl>,
    shutdown: Arc<AtomicBool>,
    incoming_generation: Arc<AtomicU64>,
    incoming_sessions: Arc<Mutex<Vec<IncomingAttempt>>>,
    outgoing_generation: Arc<AtomicU64>,
    outgoing_stream: Arc<Mutex<Option<TcpStream>>>,
    event_sender: SyncSender<CaptureEvent>,
    _threads: Mutex<Vec<JoinHandle<()>>>,
}

impl Service {
    pub fn start(settings: Settings) -> Result<Arc<Self>, String> {
        let enabled = settings.enabled;
        let edge = settings.peer_edge;
        let edge_delay = settings.edge_delay_ms;
        let listen_port = settings.listen_port;
        let settings = Arc::new(RwLock::new(settings));
        let peers = Arc::new(RwLock::new(HashMap::new()));
        let status = Arc::new(RwLock::new(if enabled {
            RuntimeStatus {
                state: "searching".into(),
                message: "正在局域网中寻找设备…".into(),
                active_peer: None,
                latency_ms: None,
                listening: true,
                last_error: None,
            }
        } else {
            RuntimeStatus::paused()
        }));
        let capture_control = Arc::new(CaptureControl::new(enabled, edge, edge_delay));
        let shutdown = Arc::new(AtomicBool::new(false));
        let incoming_generation = Arc::new(AtomicU64::new(1));
        let incoming_sessions = Arc::new(Mutex::new(Vec::new()));
        let outgoing_generation = Arc::new(AtomicU64::new(1));
        let outgoing_stream = Arc::new(Mutex::new(None));
        let session_owner = Arc::new(AtomicU8::new(SESSION_IDLE));
        let (event_sender, event_receiver) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);

        let listener = TcpListener::bind(("0.0.0.0", listen_port))
            .map_err(|error| format!("无法监听控制端口 {listen_port}：{error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("无法配置控制端口：{error}"))?;

        let service = Arc::new(Self {
            settings: Arc::clone(&settings),
            peers: Arc::clone(&peers),
            status: Arc::clone(&status),
            capture_control: Arc::clone(&capture_control),
            shutdown: Arc::clone(&shutdown),
            incoming_generation: Arc::clone(&incoming_generation),
            incoming_sessions: Arc::clone(&incoming_sessions),
            outgoing_generation: Arc::clone(&outgoing_generation),
            outgoing_stream: Arc::clone(&outgoing_stream),
            event_sender: event_sender.clone(),
            _threads: Mutex::new(Vec::new()),
        });

        let capture_thread =
            platform::spawn_capture(event_sender.clone(), Arc::clone(&capture_control))?;
        let discovery_thread = discovery::spawn(
            Arc::clone(&settings),
            Arc::clone(&peers),
            Arc::clone(&shutdown),
        )?;
        let router_thread = spawn_router(
            event_receiver,
            Arc::clone(&shutdown),
            RouterContext {
                sender: event_sender,
                settings: Arc::clone(&settings),
                peers: Arc::clone(&peers),
                status: Arc::clone(&status),
                control: Arc::clone(&capture_control),
                session_owner: Arc::clone(&session_owner),
                outgoing_generation: Arc::clone(&outgoing_generation),
                outgoing_stream: Arc::clone(&outgoing_stream),
            },
        )?;
        let listener_thread = spawn_listener(
            listener,
            IncomingContext {
                settings: Arc::clone(&settings),
                status: Arc::clone(&status),
                control: Arc::clone(&capture_control),
                shutdown: Arc::clone(&shutdown),
                generation: Arc::clone(&incoming_generation),
                session_owner: Arc::clone(&session_owner),
            },
            Arc::clone(&incoming_sessions),
        )?;

        if let Ok(mut threads) = service._threads.lock() {
            threads.extend([
                capture_thread,
                discovery_thread,
                router_thread,
                listener_thread,
            ]);
        }
        Ok(service)
    }

    pub fn update_capture_settings(&self, settings: &Settings) {
        self.capture_control
            .update(settings.enabled, settings.peer_edge, settings.edge_delay_ms);
        if !settings.enabled {
            self.release_control();
            set_status(
                &self.status,
                "paused",
                "共享已暂停，本机输入不会发送。",
                None,
                None,
                None,
            );
        } else {
            set_status(
                &self.status,
                "searching",
                "正在局域网中寻找设备…",
                None,
                None,
                None,
            );
        }
    }

    pub fn release_control(&self) {
        self.incoming_generation.fetch_add(1, Ordering::AcqRel);
        close_incoming_streams(&self.incoming_sessions);
        let was_active = cancel_outgoing(
            &self.outgoing_generation,
            &self.outgoing_stream,
            &self.capture_control,
        );
        if was_active {
            if let Ok(settings) = self.settings.read() {
                let _ = platform::restore_local_pointer(settings.peer_edge);
            }
        }
        set_ready_status(&self.status, &self.capture_control, "输入已返回本机。");
        if self
            .event_sender
            .try_send(CaptureEvent::EmergencyRelease)
            .is_err()
        {
            self.capture_control
                .overflowed
                .store(true, Ordering::Release);
        }
    }

    pub fn peer_snapshot(&self) -> Vec<PeerInfo> {
        discovery::snapshot(&self.peers)
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.capture_control.active.store(false, Ordering::Release);
        self.capture_control.pending.store(false, Ordering::Release);
        self.incoming_generation.fetch_add(1, Ordering::AcqRel);
        close_incoming_streams(&self.incoming_sessions);
        cancel_outgoing(
            &self.outgoing_generation,
            &self.outgoing_stream,
            &self.capture_control,
        );
        for session in take_incoming_threads(&self.incoming_sessions) {
            let _ = session.join();
        }
    }
}

struct ActiveOutgoing {
    writer: SecureWriter,
    session_id: u64,
    generation: u64,
    pending_ping: Arc<AtomicU64>,
    peer_name: String,
    exit_edge: Edge,
    last_ping: Instant,
    last_pong: Instant,
}

struct RouterContext {
    sender: SyncSender<CaptureEvent>,
    settings: Arc<RwLock<Settings>>,
    peers: PeerTable,
    status: Arc<RwLock<RuntimeStatus>>,
    control: Arc<CaptureControl>,
    session_owner: Arc<AtomicU8>,
    outgoing_generation: Arc<AtomicU64>,
    outgoing_stream: Arc<Mutex<Option<TcpStream>>>,
}

struct PendingMouseMove {
    dx: i32,
    dy: i32,
    queued_at: Instant,
}

fn spawn_router(
    receiver: Receiver<CaptureEvent>,
    shutdown: Arc<AtomicBool>,
    context: RouterContext,
) -> Result<JoinHandle<()>, String> {
    thread::Builder::new()
        .name("deskbridge-input-router".into())
        .spawn(move || {
            let mut active: Option<ActiveOutgoing> = None;
            let mut pending_move: Option<PendingMouseMove> = None;
            while !shutdown.load(Ordering::Acquire) {
                if context.control.overflowed.swap(false, Ordering::AcqRel) {
                    pending_move = None;
                    finish_outgoing(
                        &mut active,
                        &context.status,
                        &context.control,
                        &context.session_owner,
                        &context.outgoing_stream,
                        Some("本机输入过快，已安全返回".into()),
                    );
                }
                let timeout = pending_move
                    .as_ref()
                    .map(|movement| {
                        MOUSE_MOVE_INTERVAL.saturating_sub(movement.queued_at.elapsed())
                    })
                    .unwrap_or(Duration::from_millis(50));
                match receiver.recv_timeout(timeout) {
                    Ok(CaptureEvent::Input(WireMessage::MouseMove { dx, dy })) => {
                        queue_mouse_move(&mut pending_move, dx, dy);
                    }
                    Ok(CaptureEvent::Input(message)) => {
                        flush_pending_move(&mut pending_move, &mut active, &context);
                        handle_router_event(CaptureEvent::Input(message), &mut active, &context);
                    }
                    Ok(event) => {
                        handle_router_event(event, &mut active, &context);
                        if active.is_none() {
                            pending_move = None;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        flush_pending_move(&mut pending_move, &mut active, &context)
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }

                if pending_move
                    .as_ref()
                    .is_some_and(|movement| movement.queued_at.elapsed() >= MOUSE_MOVE_INTERVAL)
                {
                    flush_pending_move(&mut pending_move, &mut active, &context);
                }

                if active
                    .as_ref()
                    .is_some_and(|session| session.last_pong.elapsed() >= HEARTBEAT_TIMEOUT)
                {
                    finish_outgoing(
                        &mut active,
                        &context.status,
                        &context.control,
                        &context.session_owner,
                        &context.outgoing_stream,
                        Some("对方心跳超时".into()),
                    );
                } else if let Some(session) = active.as_mut() {
                    if session.last_ping.elapsed() >= Duration::from_secs(1)
                        && session.pending_ping.load(Ordering::Acquire) == 0
                    {
                        let stamp = monotonic_ms().max(1);
                        session.pending_ping.store(stamp, Ordering::Release);
                        if let Err(error) = session.writer.send(&WireMessage::Ping(stamp)) {
                            session.pending_ping.store(0, Ordering::Release);
                            let message = format!("连接中断：{error}");
                            finish_outgoing(
                                &mut active,
                                &context.status,
                                &context.control,
                                &context.session_owner,
                                &context.outgoing_stream,
                                Some(message),
                            );
                        } else {
                            session.last_ping = Instant::now();
                        }
                    }
                }
            }
            finish_outgoing(
                &mut active,
                &context.status,
                &context.control,
                &context.session_owner,
                &context.outgoing_stream,
                None,
            );
        })
        .map_err(|error| format!("无法启动输入路由：{error}"))
}

fn queue_mouse_move(pending: &mut Option<PendingMouseMove>, dx: i32, dy: i32) {
    if dx == 0 && dy == 0 {
        return;
    }
    if let Some(movement) = pending.as_mut() {
        movement.dx = movement.dx.saturating_add(dx);
        movement.dy = movement.dy.saturating_add(dy);
    } else {
        *pending = Some(PendingMouseMove {
            dx,
            dy,
            queued_at: Instant::now(),
        });
    }
}

fn flush_pending_move(
    pending: &mut Option<PendingMouseMove>,
    active: &mut Option<ActiveOutgoing>,
    context: &RouterContext,
) {
    let RouterContext {
        status,
        control,
        session_owner,
        outgoing_generation,
        outgoing_stream,
        ..
    } = context;
    let Some(movement) = pending.take() else {
        return;
    };
    if movement.dx == 0 && movement.dy == 0 {
        return;
    }
    if !control.active.load(Ordering::Acquire)
        || active
            .as_ref()
            .is_none_or(|session| session.generation != outgoing_generation.load(Ordering::Acquire))
    {
        return;
    }
    let result = active.as_mut().map(|session| {
        session.writer.send(&WireMessage::MouseMove {
            dx: movement.dx,
            dy: movement.dy,
        })
    });
    if let Some(Err(error)) = result {
        finish_outgoing(
            active,
            status,
            control,
            session_owner,
            outgoing_stream,
            Some(format!("发送输入失败：{error}")),
        );
    }
}

fn handle_router_event(
    event: CaptureEvent,
    active: &mut Option<ActiveOutgoing>,
    context: &RouterContext,
) {
    let RouterContext {
        sender,
        settings,
        peers,
        status,
        control,
        session_owner,
        outgoing_generation,
        outgoing_stream,
    } = context;
    match event {
        CaptureEvent::EdgeReached(exit_edge) => {
            if active.is_some()
                || !control.enabled.load(Ordering::Acquire)
                || !control.pending.load(Ordering::Acquire)
            {
                control.pending.store(false, Ordering::Release);
                return;
            }
            if session_owner
                .compare_exchange(
                    SESSION_IDLE,
                    SESSION_OUTGOING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                control.pending.store(false, Ordering::Release);
                return;
            }
            set_status(status, "connecting", "正在建立加密连接…", None, None, None);
            let attempt_generation = outgoing_generation.load(Ordering::Acquire);
            match connect_outgoing(
                exit_edge,
                sender,
                settings,
                peers,
                outgoing_generation,
                attempt_generation,
                outgoing_stream,
            ) {
                Ok(session) => {
                    if !publish_outgoing(
                        outgoing_generation,
                        attempt_generation,
                        outgoing_stream,
                        control,
                    ) {
                        close_stream(outgoing_stream);
                        release_session_owner(session_owner, SESSION_OUTGOING);
                        set_ready_status(status, control, "连接已取消。");
                        return;
                    }
                    set_status(
                        status,
                        "controlling",
                        &format!("正在控制 {}", session.peer_name),
                        Some(session.peer_name.clone()),
                        None,
                        None,
                    );
                    *active = Some(session);
                }
                Err(ConnectError::Cancelled) => {
                    close_stream(outgoing_stream);
                    release_session_owner(session_owner, SESSION_OUTGOING);
                    control.active.store(false, Ordering::Release);
                    control.pending.store(false, Ordering::Release);
                    set_ready_status(status, control, "连接已取消。");
                }
                Err(ConnectError::Failed(error)) => {
                    close_stream(outgoing_stream);
                    release_session_owner(session_owner, SESSION_OUTGOING);
                    control.active.store(false, Ordering::Release);
                    control.pending.store(false, Ordering::Release);
                    set_status(
                        status,
                        "error",
                        "无法连接对方，鼠标仍在本机。",
                        None,
                        None,
                        Some(error),
                    );
                }
            }
        }
        CaptureEvent::Input(message) => {
            if control.active.load(Ordering::Acquire)
                && active.as_ref().is_some_and(|session| {
                    session.generation == outgoing_generation.load(Ordering::Acquire)
                })
            {
                let session = active.as_mut().expect("active session checked above");
                if let Err(error) = session.writer.send(&message) {
                    finish_outgoing(
                        active,
                        status,
                        control,
                        session_owner,
                        outgoing_stream,
                        Some(format!("发送输入失败：{error}")),
                    );
                }
            }
        }
        CaptureEvent::EmergencyRelease => {
            finish_outgoing(
                active,
                status,
                control,
                session_owner,
                outgoing_stream,
                None,
            );
        }
        CaptureEvent::PeerReturned { session_id } => {
            if active
                .as_ref()
                .is_some_and(|session| session.session_id == session_id)
            {
                finish_outgoing(
                    active,
                    status,
                    control,
                    session_owner,
                    outgoing_stream,
                    None,
                );
            }
        }
        CaptureEvent::PeerDisconnected { session_id, error } => {
            if active
                .as_ref()
                .is_some_and(|session| session.session_id == session_id)
            {
                finish_outgoing(
                    active,
                    status,
                    control,
                    session_owner,
                    outgoing_stream,
                    Some(error),
                );
            }
        }
        CaptureEvent::PeerPong { session_id, stamp } => {
            if let Some(session) = active
                .as_mut()
                .filter(|session| session.session_id == session_id)
            {
                session.last_pong = Instant::now();
                let latency = monotonic_ms().saturating_sub(stamp);
                set_status(
                    status,
                    "controlling",
                    &format!("正在控制 {} · {latency} ms", session.peer_name),
                    Some(session.peer_name.clone()),
                    Some(latency),
                    None,
                );
            }
        }
    }
}

enum ConnectError {
    Cancelled,
    Failed(String),
}

fn connect_error(generation: &AtomicU64, attempt_generation: u64, message: String) -> ConnectError {
    if outgoing_cancelled(generation, attempt_generation) {
        ConnectError::Cancelled
    } else {
        ConnectError::Failed(message)
    }
}

fn outgoing_cancelled(generation: &AtomicU64, attempt_generation: u64) -> bool {
    generation.load(Ordering::Acquire) != attempt_generation
}

fn publish_outgoing(
    generation: &AtomicU64,
    attempt_generation: u64,
    stream_slot: &Mutex<Option<TcpStream>>,
    control: &CaptureControl,
) -> bool {
    let Ok(_transition) = control.transition.lock() else {
        return false;
    };
    let Ok(stream_slot) = stream_slot.lock() else {
        return false;
    };
    if stream_slot.is_none()
        || outgoing_cancelled(generation, attempt_generation)
        || !control.enabled.load(Ordering::Acquire)
        || !control.pending.load(Ordering::Acquire)
    {
        return false;
    }
    control.active.store(true, Ordering::Release);
    control.pending.store(false, Ordering::Release);
    true
}

fn connect_outgoing(
    exit_edge: Edge,
    event_sender: &SyncSender<CaptureEvent>,
    settings: &Arc<RwLock<Settings>>,
    peers: &PeerTable,
    outgoing_generation: &AtomicU64,
    attempt_generation: u64,
    outgoing_stream: &Mutex<Option<TcpStream>>,
) -> Result<ActiveOutgoing, ConnectError> {
    let settings_snapshot = settings
        .read()
        .map_err(|_| ConnectError::Failed("配置状态不可用".into()))?
        .clone();
    let target = resolve_target(&settings_snapshot, peers).map_err(ConnectError::Failed)?;
    if outgoing_cancelled(outgoing_generation, attempt_generation) {
        return Err(ConnectError::Cancelled);
    }
    let stream = TcpStream::connect_timeout(&target.address, Duration::from_millis(1400)).map_err(
        |error| {
            connect_error(
                outgoing_generation,
                attempt_generation,
                format!("连接 {} 失败：{error}", target.address),
            )
        },
    )?;
    let cancellation_stream = stream.try_clone().map_err(|error| {
        connect_error(
            outgoing_generation,
            attempt_generation,
            format!("无法监视出站连接：{error}"),
        )
    })?;
    {
        let mut active_stream = outgoing_stream
            .lock()
            .map_err(|_| ConnectError::Failed("连接状态不可用".into()))?;
        if outgoing_cancelled(outgoing_generation, attempt_generation) {
            return Err(ConnectError::Cancelled);
        }
        *active_stream = Some(cancellation_stream);
    }
    let key = settings_snapshot
        .pairing_key_bytes()
        .map_err(ConnectError::Failed)?;
    let mut session = protocol::client_handshake(stream, &settings_snapshot.node_id, &key)
        .map_err(|error| {
            connect_error(
                outgoing_generation,
                attempt_generation,
                format!("安全握手失败，请检查两端配对密钥：{error}"),
            )
        })?;
    if outgoing_cancelled(outgoing_generation, attempt_generation) {
        return Err(ConnectError::Cancelled);
    }
    if let Some(expected) = target.expected_id.as_deref() {
        if session.peer_id != expected {
            return Err(ConnectError::Failed("对方设备身份与所选设备不一致".into()));
        }
    }
    session
        .writer
        .send(&WireMessage::Activate {
            entry_edge: exit_edge.opposite(),
        })
        .map_err(|error| {
            connect_error(
                outgoing_generation,
                attempt_generation,
                format!("无法激活对方设备：{error}"),
            )
        })?;
    match session.reader.receive() {
        Ok(WireMessage::ActivateAck) => {}
        Ok(_) => {
            return Err(connect_error(
                outgoing_generation,
                attempt_generation,
                "对方返回了无效的激活响应".into(),
            ))
        }
        Err(error) => {
            return Err(connect_error(
                outgoing_generation,
                attempt_generation,
                format!("对方未接受控制：{error}"),
            ))
        }
    }
    if outgoing_cancelled(outgoing_generation, attempt_generation) {
        return Err(ConnectError::Cancelled);
    }

    let mut reader = session.reader;
    let sender = event_sender.clone();
    let session_id = next_outgoing_session_id();
    let pending_ping = Arc::new(AtomicU64::new(0));
    let reader_pending_ping = Arc::clone(&pending_ping);
    thread::Builder::new()
        .name("deskbridge-peer-control".into())
        .spawn(move || loop {
            match reader.receive() {
                Ok(WireMessage::ReturnControl) => {
                    let _ = sender.send(CaptureEvent::PeerReturned { session_id });
                    break;
                }
                Ok(WireMessage::Pong(stamp)) => {
                    if stamp != 0
                        && reader_pending_ping
                            .compare_exchange(stamp, 0, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                    {
                        let _ = sender.send(CaptureEvent::PeerPong { session_id, stamp });
                    } else {
                        let _ = sender.send(CaptureEvent::PeerDisconnected {
                            session_id,
                            error: "对方返回了无效心跳".into(),
                        });
                        break;
                    }
                }
                Ok(_) => {
                    let _ = sender.send(CaptureEvent::PeerDisconnected {
                        session_id,
                        error: "对方返回了当前会话不允许的消息".into(),
                    });
                    break;
                }
                Err(error) => {
                    let _ = sender.send(CaptureEvent::PeerDisconnected {
                        session_id,
                        error: format!("对方已断开：{error}"),
                    });
                    break;
                }
            }
        })
        .map_err(|error| {
            connect_error(
                outgoing_generation,
                attempt_generation,
                format!("无法启动连接监视器：{error}"),
            )
        })?;

    Ok(ActiveOutgoing {
        writer: session.writer,
        session_id,
        generation: attempt_generation,
        pending_ping,
        peer_name: target.name,
        exit_edge,
        last_ping: Instant::now(),
        last_pong: Instant::now(),
    })
}

fn finish_outgoing(
    active: &mut Option<ActiveOutgoing>,
    status: &Arc<RwLock<RuntimeStatus>>,
    control: &Arc<CaptureControl>,
    session_owner: &AtomicU8,
    outgoing_stream: &Mutex<Option<TcpStream>>,
    error: Option<String>,
) {
    {
        let _transition = control.transition.lock().ok();
        control.active.store(false, Ordering::Release);
        control.pending.store(false, Ordering::Release);
    }
    if let Some(mut session) = active.take() {
        let _ = platform::restore_local_pointer(session.exit_edge);
        let _ = session.writer.send(&WireMessage::ReleaseAll);
    }
    close_stream(outgoing_stream);
    release_session_owner(session_owner, SESSION_OUTGOING);
    if control.enabled.load(Ordering::Acquire) {
        match error {
            Some(error) => set_status(
                status,
                "error",
                "连接中断，输入已自动返回本机。",
                None,
                None,
                Some(error),
            ),
            None => set_status(status, "ready", "输入已返回本机。", None, None, None),
        }
    }
}

fn set_ready_status(status: &Arc<RwLock<RuntimeStatus>>, control: &CaptureControl, message: &str) {
    if control.enabled.load(Ordering::Acquire) {
        set_status(status, "ready", message, None, None, None);
    }
}

struct Target {
    address: SocketAddr,
    expected_id: Option<String>,
    name: String,
}

fn resolve_target(settings: &Settings, peers: &PeerTable) -> Result<Target, String> {
    if let Some(peer_id) = settings.selected_peer_id.as_deref() {
        if let Some(peer) = peers
            .read()
            .ok()
            .and_then(|table| table.get(peer_id).cloned())
        {
            let ip: IpAddr = peer
                .address
                .parse()
                .map_err(|_| "发现到的设备地址无效".to_string())?;
            if !is_lan_ip(ip) {
                return Err("所选设备不在本地局域网地址范围内".into());
            }
            return Ok(Target {
                address: SocketAddr::new(ip, peer.port),
                expected_id: Some(peer.node_id),
                name: peer.name,
            });
        }
    }

    if settings.manual_address.is_empty() {
        return Err("尚未选择在线设备，也没有填写手动 IP".into());
    }
    let with_port = add_default_port(&settings.manual_address, settings.listen_port);
    let address = with_port
        .to_socket_addrs()
        .map_err(|error| format!("无法解析手动地址：{error}"))?
        .find(|address| address.is_ipv4() && is_lan_ip(address.ip()))
        .ok_or_else(|| "手动地址没有可用的局域网 IPv4 地址".to_string())?;
    Ok(Target {
        address,
        expected_id: settings.selected_peer_id.clone(),
        name: settings.manual_address.clone(),
    })
}

fn add_default_port(value: &str, port: u16) -> String {
    if value.parse::<SocketAddr>().is_ok() {
        value.to_string()
    } else if value.parse::<IpAddr>().is_ok() {
        format!("{value}:{port}")
    } else if value
        .rsplit_once(':')
        .is_some_and(|(_, suffix)| suffix.parse::<u16>().is_ok())
    {
        value.to_string()
    } else {
        format!("{value}:{port}")
    }
}

fn is_lan_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_private() || address.is_loopback() || address.is_link_local()
        }
        IpAddr::V6(_) => false,
    }
}

struct IncomingAttempt {
    source: IpAddr,
    stream: TcpStream,
    thread: JoinHandle<()>,
}

fn spawn_listener(
    listener: TcpListener,
    incoming: IncomingContext,
    incoming_sessions: Arc<Mutex<Vec<IncomingAttempt>>>,
) -> Result<JoinHandle<()>, String> {
    thread::Builder::new()
        .name("deskbridge-listener".into())
        .spawn(move || {
            while !incoming.shutdown.load(Ordering::Acquire) {
                reap_finished_threads(&incoming_sessions);
                match listener.accept() {
                    Ok((stream, address)) => {
                        if !is_lan_ip(address.ip()) {
                            drop(stream);
                            continue;
                        }
                        let Ok(mut session_slots) = incoming_sessions.lock() else {
                            drop(stream);
                            continue;
                        };
                        if incoming.shutdown.load(Ordering::Acquire)
                            || session_slots.len() >= MAX_PREAUTH_CONNECTIONS
                            || session_slots
                                .iter()
                                .any(|attempt| attempt.source == address.ip())
                        {
                            drop(stream);
                            continue;
                        }
                        let accepted_generation = incoming.generation.load(Ordering::Acquire);
                        let session_context = incoming.clone();
                        let Ok(cancellation_stream) = stream.try_clone() else {
                            drop(stream);
                            continue;
                        };
                        if let Ok(session) = thread::Builder::new()
                            .name("deskbridge-receiver-session".into())
                            .spawn(move || {
                                handle_incoming(stream, session_context, accepted_generation)
                            })
                        {
                            session_slots.push(IncomingAttempt {
                                source: address.ip(),
                                stream: cancellation_stream,
                                thread: session,
                            });
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(60));
                    }
                    Err(error) => {
                        set_status(
                            &incoming.status,
                            "error",
                            "局域网监听发生错误。",
                            None,
                            None,
                            Some(error.to_string()),
                        );
                        thread::sleep(Duration::from_millis(250));
                    }
                }
            }
        })
        .map_err(|error| format!("无法启动连接监听：{error}"))
}

#[derive(Clone)]
struct IncomingContext {
    settings: Arc<RwLock<Settings>>,
    status: Arc<RwLock<RuntimeStatus>>,
    control: Arc<CaptureControl>,
    shutdown: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    session_owner: Arc<AtomicU8>,
}

fn handle_incoming(stream: TcpStream, context: IncomingContext, accepted_generation: u64) {
    let IncomingContext {
        settings,
        status,
        control,
        shutdown,
        generation: incoming_generation,
        session_owner,
    } = context;
    if incoming_session_cancelled(&incoming_generation, accepted_generation, &control) {
        return;
    }
    let settings_snapshot = match settings.read() {
        Ok(settings) => settings.clone(),
        Err(_) => return,
    };
    if !settings_snapshot.enabled || !platform::permission_state().injection {
        return;
    }
    let key = match settings_snapshot.pairing_key_bytes() {
        Ok(key) => key,
        Err(_) => return,
    };
    let mut session = match protocol::server_handshake(stream, &settings_snapshot.node_id, &key) {
        Ok(session) => session,
        Err(_) => return,
    };
    if incoming_session_cancelled(&incoming_generation, accepted_generation, &control) {
        return;
    }
    if settings_snapshot
        .selected_peer_id
        .as_deref()
        .is_some_and(|expected| expected != session.peer_id)
    {
        return;
    }
    if session_owner
        .compare_exchange(
            SESSION_IDLE,
            SESSION_INCOMING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return;
    }
    let _session_guard = SessionOwnerGuard {
        owner: session_owner,
        expected: SESSION_INCOMING,
    };

    let mut injector = Injector::new();
    let mut activated = false;
    let mut last_permission_check = Instant::now();
    while !shutdown.load(Ordering::Acquire) {
        if incoming_session_cancelled(&incoming_generation, accepted_generation, &control) {
            break;
        }
        if last_permission_check.elapsed() >= Duration::from_secs(1) {
            if !platform::permission_state().injection {
                break;
            }
            last_permission_check = Instant::now();
        }
        let received = session.reader.receive();
        if incoming_session_cancelled(&incoming_generation, accepted_generation, &control) {
            break;
        }
        match received {
            Ok(WireMessage::Activate { entry_edge }) if !activated => {
                if injector.activate(entry_edge).is_err() {
                    break;
                }
                activated = true;
                set_status(
                    &status,
                    "controlled",
                    "正在接收对方的鼠标和键盘。",
                    Some(session.peer_id.clone()),
                    None,
                    None,
                );
                if session.writer.send(&WireMessage::ActivateAck).is_err() {
                    break;
                }
            }
            Ok(WireMessage::Ping(stamp)) => {
                if session.writer.send(&WireMessage::Pong(stamp)).is_err() {
                    break;
                }
            }
            Ok(WireMessage::ReleaseAll) => {
                injector.release_all();
                break;
            }
            Ok(message) if activated => match injector.apply(&message) {
                Ok(InjectionOutcome::Continue) => {}
                Ok(InjectionOutcome::ReturnRequested) => {
                    let _ = session.writer.send(&WireMessage::ReturnControl);
                    injector.release_all();
                    break;
                }
                Err(error) => {
                    set_status(
                        &status,
                        "error",
                        "输入注入失败，已停止接收控制。",
                        None,
                        None,
                        Some(error),
                    );
                    break;
                }
            },
            Ok(_) => break,
            Err(_) => break,
        }
    }
    injector.release_all();
    if control.enabled.load(Ordering::Acquire) {
        set_status(&status, "ready", "对方控制已结束。", None, None, None);
    }
}

fn incoming_session_cancelled(
    generation: &AtomicU64,
    accepted_generation: u64,
    control: &CaptureControl,
) -> bool {
    generation.load(Ordering::Acquire) != accepted_generation
        || !control.enabled.load(Ordering::Acquire)
}

struct SessionOwnerGuard {
    owner: Arc<AtomicU8>,
    expected: u8,
}

impl Drop for SessionOwnerGuard {
    fn drop(&mut self) {
        release_session_owner(&self.owner, self.expected);
    }
}

fn release_session_owner(owner: &AtomicU8, expected: u8) {
    let _ = owner.compare_exchange(expected, SESSION_IDLE, Ordering::AcqRel, Ordering::Acquire);
}

fn close_stream(stream: &Mutex<Option<TcpStream>>) {
    if let Ok(mut stream) = stream.lock() {
        if let Some(stream) = stream.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

fn close_incoming_streams(attempts: &Mutex<Vec<IncomingAttempt>>) {
    if let Ok(attempts) = attempts.lock() {
        for attempt in attempts.iter() {
            let _ = attempt.stream.shutdown(Shutdown::Both);
        }
    }
}

fn cancel_outgoing(
    generation: &AtomicU64,
    stream_slot: &Mutex<Option<TcpStream>>,
    control: &CaptureControl,
) -> bool {
    let _transition = control.transition.lock().ok();
    control.pending.store(false, Ordering::Release);
    match stream_slot.lock() {
        Ok(mut stream_slot) => {
            generation.fetch_add(1, Ordering::AcqRel);
            let was_active = control.active.swap(false, Ordering::AcqRel);
            if let Some(stream) = stream_slot.take() {
                let _ = stream.shutdown(Shutdown::Both);
            }
            was_active
        }
        Err(_) => {
            generation.fetch_add(1, Ordering::AcqRel);
            control.active.swap(false, Ordering::AcqRel)
        }
    }
}

fn take_incoming_threads(slot: &Mutex<Vec<IncomingAttempt>>) -> Vec<JoinHandle<()>> {
    slot.lock()
        .map(|mut attempts| {
            std::mem::take(&mut *attempts)
                .into_iter()
                .map(|attempt| attempt.thread)
                .collect()
        })
        .unwrap_or_default()
}

fn reap_finished_threads(slot: &Mutex<Vec<IncomingAttempt>>) {
    let mut finished = Vec::new();
    if let Ok(mut attempts) = slot.lock() {
        let mut index = 0;
        while index < attempts.len() {
            if attempts[index].thread.is_finished() {
                finished.push(attempts.swap_remove(index).thread);
            } else {
                index += 1;
            }
        }
    }
    for thread in finished {
        let _ = thread.join();
    }
}

fn set_status(
    status: &Arc<RwLock<RuntimeStatus>>,
    state: &str,
    message: &str,
    active_peer: Option<String>,
    latency_ms: Option<u64>,
    error: Option<String>,
) {
    if let Ok(mut status) = status.write() {
        status.state = state.into();
        status.message = message.into();
        status.active_peer = active_peer;
        status.latency_ms = latency_ms;
        status.listening = true;
        status.last_error = error;
    }
}

fn monotonic_ms() -> u64 {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    ORIGIN
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn next_outgoing_session_id() -> u64 {
    static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incoming_generation_invalidates_existing_session() {
        let generation = AtomicU64::new(7);
        let control = CaptureControl::new(true, Edge::Right, 180);
        assert!(!incoming_session_cancelled(&generation, 7, &control));

        generation.fetch_add(1, Ordering::Release);
        assert!(incoming_session_cancelled(&generation, 7, &control));
    }

    #[test]
    fn disabling_sharing_invalidates_incoming_session() {
        let generation = AtomicU64::new(3);
        let control = CaptureControl::new(true, Edge::Right, 180);
        control.enabled.store(false, Ordering::Release);
        assert!(incoming_session_cancelled(&generation, 3, &control));
    }

    #[test]
    fn outgoing_session_ids_are_unique() {
        assert_ne!(next_outgoing_session_id(), next_outgoing_session_id());
    }

    #[test]
    fn mouse_moves_are_coalesced_without_overflow() {
        let mut pending = None;
        queue_mouse_move(&mut pending, i32::MAX, 4);
        queue_mouse_move(&mut pending, 12, -9);
        let movement = pending.unwrap();
        assert_eq!(movement.dx, i32::MAX);
        assert_eq!(movement.dy, -5);
    }

    #[test]
    fn incoming_and_outgoing_sessions_are_mutually_exclusive() {
        let owner = AtomicU8::new(SESSION_IDLE);
        assert!(owner
            .compare_exchange(
                SESSION_IDLE,
                SESSION_OUTGOING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok());
        assert!(owner
            .compare_exchange(
                SESSION_IDLE,
                SESSION_INCOMING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err());
        release_session_owner(&owner, SESSION_OUTGOING);
        assert_eq!(owner.load(Ordering::Acquire), SESSION_IDLE);
    }

    #[test]
    fn only_local_ipv4_ranges_are_accepted() {
        assert!(is_lan_ip("192.168.1.10".parse().unwrap()));
        assert!(is_lan_ip("169.254.8.4".parse().unwrap()));
        assert!(is_lan_ip("127.0.0.1".parse().unwrap()));
        assert!(!is_lan_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_lan_ip("2001:4860:4860::8888".parse().unwrap()));
    }
}
