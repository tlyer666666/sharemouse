use super::{try_send_capture, CaptureControl, CaptureEvent, PermissionState};
use crate::config::Edge;
use crate::protocol::WireMessage;
use std::collections::HashSet;
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicPtr, AtomicU8, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFStringRef = *const c_void;

const KCG_EVENT_LEFT_MOUSE_DOWN: u32 = 1;
const KCG_EVENT_LEFT_MOUSE_UP: u32 = 2;
const KCG_EVENT_RIGHT_MOUSE_DOWN: u32 = 3;
const KCG_EVENT_RIGHT_MOUSE_UP: u32 = 4;
const KCG_EVENT_MOUSE_MOVED: u32 = 5;
const KCG_EVENT_LEFT_MOUSE_DRAGGED: u32 = 6;
const KCG_EVENT_RIGHT_MOUSE_DRAGGED: u32 = 7;
const KCG_EVENT_KEY_DOWN: u32 = 10;
const KCG_EVENT_KEY_UP: u32 = 11;
const KCG_EVENT_FLAGS_CHANGED: u32 = 12;
const KCG_EVENT_SCROLL_WHEEL: u32 = 22;
const KCG_EVENT_OTHER_MOUSE_DOWN: u32 = 25;
const KCG_EVENT_OTHER_MOUSE_UP: u32 = 26;
const KCG_EVENT_OTHER_MOUSE_DRAGGED: u32 = 27;
const KCG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const KCG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
const KCG_MOUSE_EVENT_BUTTON_NUMBER: u32 = 3;
const KCG_MOUSE_EVENT_DELTA_X: u32 = 4;
const KCG_MOUSE_EVENT_DELTA_Y: u32 = 5;
const KCG_KEYBOARD_EVENT_KEYCODE: u32 = 9;
const KCG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1: u32 = 11;
const KCG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2: u32 = 12;
const KCG_EVENT_SOURCE_USER_DATA: u32 = 55;
const KCG_HID_EVENT_TAP: u32 = 0;
const KCG_SESSION_EVENT_TAP: u32 = 1;
const KCG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const KCG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;
const KCG_EVENT_SOURCE_STATE_COMBINED_SESSION: i32 = 0;
const KCG_SCROLL_EVENT_UNIT_PIXEL: u32 = 0;
const INJECTION_SENTINEL: i64 = 0x4453_4B42;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: Option<
            unsafe extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef,
        >,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventSourceButtonState(state_id: i32, button: u32) -> bool;
    fn CGEventSourceKeyState(state_id: i32, virtual_key: u16) -> bool;
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventCreate(source: *mut c_void) -> CGEventRef;
    fn CGEventCreateMouseEvent(
        source: *mut c_void,
        kind: u32,
        point: CGPoint,
        button: u32,
    ) -> CGEventRef;
    fn CGEventCreateKeyboardEvent(source: *mut c_void, virtual_key: u16, down: bool) -> CGEventRef;
    fn CGEventCreateScrollWheelEvent2(
        source: *mut c_void,
        units: u32,
        wheel_count: u32,
        wheel_1: i32,
        wheel_2: i32,
        wheel_3: i32,
    ) -> CGEventRef;
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGWarpMouseCursorPosition(point: CGPoint) -> i32;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
    fn AXIsProcessTrusted() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(loop_ref: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRun();
    fn CFRelease(object: *const c_void);
}

struct HookContext {
    sender: SyncSender<CaptureEvent>,
    control: Arc<CaptureControl>,
    state: Mutex<HookState>,
}

#[derive(Default)]
struct HookState {
    edge_since: Option<Instant>,
    edge_latched: bool,
    was_active: bool,
    pressed_buttons: HashSet<u8>,
    pressed_keys: HashSet<u16>,
    pressed_modifiers: HashSet<u16>,
}

static CONTEXT: OnceLock<HookContext> = OnceLock::new();
static EVENT_TAP: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static INJECTED_BUTTONS: AtomicU8 = AtomicU8::new(0);

pub fn spawn_capture(
    sender: SyncSender<CaptureEvent>,
    control: Arc<CaptureControl>,
) -> Result<JoinHandle<()>, String> {
    CONTEXT
        .set(HookContext {
            sender,
            control,
            state: Mutex::new(HookState::default()),
        })
        .map_err(|_| "输入捕获服务已经启动".to_string())?;
    thread::Builder::new()
        .name("deskbridge-macos-event-tap".into())
        .spawn(event_tap_thread)
        .map_err(|error| format!("无法启动 macOS 输入捕获：{error}"))
}

fn event_tap_thread() {
    let event_types = [
        KCG_EVENT_LEFT_MOUSE_DOWN,
        KCG_EVENT_LEFT_MOUSE_UP,
        KCG_EVENT_RIGHT_MOUSE_DOWN,
        KCG_EVENT_RIGHT_MOUSE_UP,
        KCG_EVENT_MOUSE_MOVED,
        KCG_EVENT_LEFT_MOUSE_DRAGGED,
        KCG_EVENT_RIGHT_MOUSE_DRAGGED,
        KCG_EVENT_KEY_DOWN,
        KCG_EVENT_KEY_UP,
        KCG_EVENT_FLAGS_CHANGED,
        KCG_EVENT_SCROLL_WHEEL,
        KCG_EVENT_OTHER_MOUSE_DOWN,
        KCG_EVENT_OTHER_MOUSE_UP,
        KCG_EVENT_OTHER_MOUSE_DRAGGED,
    ];
    let mask = event_types
        .iter()
        .fold(0_u64, |mask, kind| mask | (1_u64 << kind));
    loop {
        unsafe {
            let tap = CGEventTapCreate(
                KCG_SESSION_EVENT_TAP,
                KCG_HEAD_INSERT_EVENT_TAP,
                KCG_EVENT_TAP_OPTION_DEFAULT,
                mask,
                Some(event_callback),
                null_mut(),
            );
            if tap.is_null() {
                thread::sleep(Duration::from_secs(2));
                continue;
            }
            EVENT_TAP.store(tap, Ordering::Release);
            let source = CFMachPortCreateRunLoopSource(null(), tap, 0);
            if source.is_null() {
                EVENT_TAP.store(null_mut(), Ordering::Release);
                CFRelease(tap);
                thread::sleep(Duration::from_secs(2));
                continue;
            }
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);
            CFRunLoopRun();
            EVENT_TAP.store(null_mut(), Ordering::Release);
            CFRelease(source);
            CFRelease(tap);
        }
        thread::sleep(Duration::from_millis(250));
    }
}

unsafe extern "C" fn event_callback(
    _proxy: CGEventTapProxy,
    kind: u32,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    let Some(context) = CONTEXT.get() else {
        return event;
    };
    if kind == KCG_EVENT_TAP_DISABLED_BY_TIMEOUT || kind == KCG_EVENT_TAP_DISABLED_BY_USER_INPUT {
        if let Ok(_transition) = context.control.transition.lock() {
            context.control.active.store(false, Ordering::Release);
            context.control.pending.store(false, Ordering::Release);
            context.control.overflowed.store(true, Ordering::Release);
            if let Ok(mut state) = context.state.lock() {
                reset_capture_state(&mut state);
            }
            send_event(context, CaptureEvent::EmergencyRelease);
        }
        let tap = EVENT_TAP.load(Ordering::Acquire);
        if !tap.is_null() {
            CGEventTapEnable(tap, true);
        }
        return event;
    }
    if CGEventGetIntegerValueField(event, KCG_EVENT_SOURCE_USER_DATA) == INJECTION_SENTINEL {
        return event;
    }
    let Ok(_transition) = context.control.transition.lock() else {
        return event;
    };
    let enabled = context.control.enabled.load(Ordering::Acquire);
    let active = enabled && context.control.active.load(Ordering::Acquire);
    if is_mouse_event(kind) {
        if let Ok(mut state) = context.state.lock() {
            let button_changed = update_button_state(&mut state, kind, event);
            if !enabled {
                state.edge_since = None;
                state.edge_latched = false;
                state.was_active = false;
                return event;
            }
            if active {
                state.was_active = true;
                if let Some(message) = mouse_message(kind, event) {
                    send_event(context, CaptureEvent::Input(message));
                }
                return null_mut();
            }
            state.was_active = false;
            if context.control.pending.load(Ordering::Acquire) {
                let moved_off_edge = kind == KCG_EVENT_MOUSE_MOVED
                    && !on_activation_edge(context, CGEventGetLocation(event));
                if button_changed || kind == KCG_EVENT_SCROLL_WHEEL || moved_off_edge {
                    context.control.pending.store(false, Ordering::Release);
                    send_event(context, CaptureEvent::EmergencyRelease);
                }
                return event;
            }
            if kind == KCG_EVENT_MOUSE_MOVED
                && state.pressed_buttons.is_empty()
                && state.pressed_keys.is_empty()
            {
                watch_edge(context, &mut state, CGEventGetLocation(event));
            }
        }
        return event;
    }

    if is_keyboard_event(kind) {
        let keycode = CGEventGetIntegerValueField(event, KCG_KEYBOARD_EVENT_KEYCODE) as u16;
        let usage = mac_keycode_to_hid(keycode);
        let mut down = kind == KCG_EVENT_KEY_DOWN;
        let mut emergency = false;
        if let (Some(usage), Ok(mut state)) = (usage, context.state.lock()) {
            if kind == KCG_EVENT_FLAGS_CHANGED {
                down = CGEventSourceKeyState(KCG_EVENT_SOURCE_STATE_COMBINED_SESSION, keycode);
            }
            if down {
                state.pressed_keys.insert(usage);
            } else {
                state.pressed_keys.remove(&usage);
            }
            if is_modifier(usage) {
                if down {
                    state.pressed_modifiers.insert(usage);
                } else {
                    state.pressed_modifiers.remove(&usage);
                }
            }
            emergency = down && usage == 0x29 && has_emergency_modifiers(&state.pressed_modifiers);
        }
        if !enabled {
            return event;
        }
        if !active {
            if context.control.pending.swap(false, Ordering::AcqRel) {
                send_event(context, CaptureEvent::EmergencyRelease);
            }
            return event;
        }
        if emergency {
            context.control.active.store(false, Ordering::Release);
            context.control.pending.store(false, Ordering::Release);
            send_event(context, CaptureEvent::EmergencyRelease);
            return null_mut();
        }
        let Some(usage) = usage else {
            return event;
        };
        send_event(
            context,
            CaptureEvent::Input(WireMessage::Key { usage, down }),
        );
        return null_mut();
    }
    event
}

fn is_mouse_event(kind: u32) -> bool {
    matches!(
        kind,
        KCG_EVENT_LEFT_MOUSE_DOWN
            | KCG_EVENT_LEFT_MOUSE_UP
            | KCG_EVENT_RIGHT_MOUSE_DOWN
            | KCG_EVENT_RIGHT_MOUSE_UP
            | KCG_EVENT_MOUSE_MOVED
            | KCG_EVENT_LEFT_MOUSE_DRAGGED
            | KCG_EVENT_RIGHT_MOUSE_DRAGGED
            | KCG_EVENT_SCROLL_WHEEL
            | KCG_EVENT_OTHER_MOUSE_DOWN
            | KCG_EVENT_OTHER_MOUSE_UP
            | KCG_EVENT_OTHER_MOUSE_DRAGGED
    )
}

fn is_keyboard_event(kind: u32) -> bool {
    matches!(
        kind,
        KCG_EVENT_KEY_DOWN | KCG_EVENT_KEY_UP | KCG_EVENT_FLAGS_CHANGED
    )
}

fn mouse_message(kind: u32, event: CGEventRef) -> Option<WireMessage> {
    unsafe {
        match kind {
            KCG_EVENT_MOUSE_MOVED
            | KCG_EVENT_LEFT_MOUSE_DRAGGED
            | KCG_EVENT_RIGHT_MOUSE_DRAGGED
            | KCG_EVENT_OTHER_MOUSE_DRAGGED => Some(WireMessage::MouseMove {
                dx: CGEventGetIntegerValueField(event, KCG_MOUSE_EVENT_DELTA_X) as i32,
                dy: CGEventGetIntegerValueField(event, KCG_MOUSE_EVENT_DELTA_Y) as i32,
            }),
            KCG_EVENT_LEFT_MOUSE_DOWN => Some(WireMessage::MouseButton {
                button: 1,
                down: true,
            }),
            KCG_EVENT_LEFT_MOUSE_UP => Some(WireMessage::MouseButton {
                button: 1,
                down: false,
            }),
            KCG_EVENT_RIGHT_MOUSE_DOWN => Some(WireMessage::MouseButton {
                button: 2,
                down: true,
            }),
            KCG_EVENT_RIGHT_MOUSE_UP => Some(WireMessage::MouseButton {
                button: 2,
                down: false,
            }),
            KCG_EVENT_OTHER_MOUSE_DOWN => Some(WireMessage::MouseButton {
                button: (CGEventGetIntegerValueField(event, KCG_MOUSE_EVENT_BUTTON_NUMBER) + 1)
                    as u8,
                down: true,
            }),
            KCG_EVENT_OTHER_MOUSE_UP => Some(WireMessage::MouseButton {
                button: (CGEventGetIntegerValueField(event, KCG_MOUSE_EVENT_BUTTON_NUMBER) + 1)
                    as u8,
                down: false,
            }),
            KCG_EVENT_SCROLL_WHEEL => Some(WireMessage::Wheel {
                horizontal: CGEventGetIntegerValueField(event, KCG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2)
                    as i32,
                vertical: CGEventGetIntegerValueField(event, KCG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1)
                    as i32,
            }),
            _ => None,
        }
    }
}

fn update_button_state(state: &mut HookState, kind: u32, event: CGEventRef) -> bool {
    let update = unsafe {
        match kind {
            KCG_EVENT_LEFT_MOUSE_DOWN => Some((1, true)),
            KCG_EVENT_LEFT_MOUSE_UP => Some((1, false)),
            KCG_EVENT_RIGHT_MOUSE_DOWN => Some((2, true)),
            KCG_EVENT_RIGHT_MOUSE_UP => Some((2, false)),
            KCG_EVENT_OTHER_MOUSE_DOWN => Some((
                (CGEventGetIntegerValueField(event, KCG_MOUSE_EVENT_BUTTON_NUMBER) + 1) as u8,
                true,
            )),
            KCG_EVENT_OTHER_MOUSE_UP => Some((
                (CGEventGetIntegerValueField(event, KCG_MOUSE_EVENT_BUTTON_NUMBER) + 1) as u8,
                false,
            )),
            _ => None,
        }
    };
    if let Some((button, down)) = update {
        if down {
            state.pressed_buttons.insert(button);
        } else {
            state.pressed_buttons.remove(&button);
        }
        true
    } else {
        false
    }
}

fn reset_capture_state(state: &mut HookState) {
    state.edge_since = None;
    state.edge_latched = false;
    state.was_active = false;
    state.pressed_buttons.clear();
    state.pressed_keys.clear();
    state.pressed_modifiers.clear();
}

fn on_activation_edge(context: &HookContext, point: CGPoint) -> bool {
    let edge = Edge::from_u8(context.control.edge.load(Ordering::Acquire));
    let bounds = main_display_bounds();
    let at_edge = match edge {
        Edge::Left => point.x <= bounds.origin.x + 1.0,
        Edge::Right => point.x >= bounds.origin.x + bounds.size.width - 2.0,
        Edge::Disabled => false,
    };
    at_edge
        && point.y > bounds.origin.y + 24.0
        && point.y < bounds.origin.y + bounds.size.height - 24.0
}

fn watch_edge(context: &HookContext, state: &mut HookState, point: CGPoint) {
    let edge = Edge::from_u8(context.control.edge.load(Ordering::Acquire));
    let bounds = main_display_bounds();
    let at_edge = match edge {
        Edge::Left => point.x <= bounds.origin.x + 1.0,
        Edge::Right => point.x >= bounds.origin.x + bounds.size.width - 2.0,
        Edge::Disabled => false,
    };
    let outside_corner_guard =
        point.y > bounds.origin.y + 24.0 && point.y < bounds.origin.y + bounds.size.height - 24.0;
    if at_edge && outside_corner_guard {
        let since = state.edge_since.get_or_insert_with(Instant::now);
        let delay = context.control.edge_delay_ms.load(Ordering::Acquire);
        if !state.edge_latched
            && since.elapsed().as_millis() >= delay as u128
            && !physical_input_down()
        {
            state.edge_latched = true;
            context.control.pending.store(true, Ordering::Release);
            send_event(context, CaptureEvent::EdgeReached(edge));
        }
    } else {
        state.edge_since = None;
        state.edge_latched = false;
    }
}

fn physical_input_down() -> bool {
    let key_down = (0_u16..=127).any(|keycode| unsafe {
        CGEventSourceKeyState(KCG_EVENT_SOURCE_STATE_COMBINED_SESSION, keycode)
    });
    key_down
        || (0_u32..=31).any(|button| unsafe {
            CGEventSourceButtonState(KCG_EVENT_SOURCE_STATE_COMBINED_SESSION, button)
        })
}

fn send_event(context: &HookContext, event: CaptureEvent) {
    try_send_capture(&context.sender, &context.control, event);
}

pub fn inject_mouse_move(dx: i32, dy: i32) -> Result<(), String> {
    let current = current_pointer()?;
    let bounds = main_display_bounds();
    let target = CGPoint {
        x: (current.x + f64::from(dx))
            .clamp(bounds.origin.x, bounds.origin.x + bounds.size.width - 1.0),
        y: (current.y + f64::from(dy))
            .clamp(bounds.origin.y, bounds.origin.y + bounds.size.height - 1.0),
    };
    let buttons = INJECTED_BUTTONS.load(Ordering::Acquire);
    let (kind, button) = if buttons & 1 != 0 {
        (KCG_EVENT_LEFT_MOUSE_DRAGGED, 0)
    } else if buttons & 2 != 0 {
        (KCG_EVENT_RIGHT_MOUSE_DRAGGED, 1)
    } else if buttons != 0 {
        (KCG_EVENT_OTHER_MOUSE_DRAGGED, 2)
    } else {
        (KCG_EVENT_MOUSE_MOVED, 0)
    };
    post_event(unsafe { CGEventCreateMouseEvent(null_mut(), kind, target, button) })
}

pub fn inject_mouse_button(button: u8, down: bool) -> Result<(), String> {
    let (kind, cg_button) = match (button, down) {
        (1, true) => (KCG_EVENT_LEFT_MOUSE_DOWN, 0),
        (1, false) => (KCG_EVENT_LEFT_MOUSE_UP, 0),
        (2, true) => (KCG_EVENT_RIGHT_MOUSE_DOWN, 1),
        (2, false) => (KCG_EVENT_RIGHT_MOUSE_UP, 1),
        (_, true) => (
            KCG_EVENT_OTHER_MOUSE_DOWN,
            u32::from(button.saturating_sub(1)),
        ),
        (_, false) => (
            KCG_EVENT_OTHER_MOUSE_UP,
            u32::from(button.saturating_sub(1)),
        ),
    };
    let mask = 1_u8
        .checked_shl(u32::from(button.saturating_sub(1)))
        .unwrap_or(0);
    post_event(unsafe {
        CGEventCreateMouseEvent(null_mut(), kind, current_pointer()?, cg_button)
    })?;
    if down {
        INJECTED_BUTTONS.fetch_or(mask, Ordering::AcqRel);
    } else {
        INJECTED_BUTTONS.fetch_and(!mask, Ordering::AcqRel);
    }
    Ok(())
}

pub fn inject_wheel(horizontal: i32, vertical: i32) -> Result<(), String> {
    post_event(unsafe {
        CGEventCreateScrollWheelEvent2(
            null_mut(),
            KCG_SCROLL_EVENT_UNIT_PIXEL,
            2,
            vertical,
            horizontal,
            0,
        )
    })
}

pub fn inject_key(usage: u16, down: bool) -> Result<(), String> {
    let Some(keycode) = hid_to_mac_keycode(usage) else {
        return Ok(());
    };
    post_event(unsafe { CGEventCreateKeyboardEvent(null_mut(), keycode, down) })
}

fn post_event(event: CGEventRef) -> Result<(), String> {
    if event.is_null() {
        return Err("macOS 无法创建输入事件；请检查辅助功能权限".into());
    }
    if !unsafe { AXIsProcessTrusted() } {
        unsafe { CFRelease(event) };
        return Err("macOS 辅助功能权限不可用".into());
    }
    unsafe {
        CGEventSetIntegerValueField(event, KCG_EVENT_SOURCE_USER_DATA, INJECTION_SENTINEL);
        CGEventPost(KCG_HID_EVENT_TAP, event);
        CFRelease(event);
    }
    Ok(())
}

fn current_pointer() -> Result<CGPoint, String> {
    unsafe {
        let event = CGEventCreate(null_mut());
        if event.is_null() {
            return Err("无法读取鼠标位置".into());
        }
        let point = CGEventGetLocation(event);
        CFRelease(event);
        Ok(point)
    }
}

pub fn place_pointer(edge: Edge) -> Result<(), String> {
    let bounds = main_display_bounds();
    let current = current_pointer().unwrap_or(CGPoint {
        x: bounds.origin.x + bounds.size.width / 2.0,
        y: bounds.origin.y + bounds.size.height / 2.0,
    });
    let point = CGPoint {
        x: match edge {
            Edge::Left => bounds.origin.x + 8.0,
            Edge::Right => bounds.origin.x + bounds.size.width - 9.0,
            Edge::Disabled => bounds.origin.x + bounds.size.width / 2.0,
        },
        y: current.y.clamp(
            bounds.origin.y + 24.0,
            bounds.origin.y + bounds.size.height - 25.0,
        ),
    };
    if unsafe { CGWarpMouseCursorPosition(point) } == 0 {
        Ok(())
    } else {
        Err("macOS 无法定位鼠标；请检查辅助功能权限".into())
    }
}

pub fn restore_local_pointer(edge: Edge) -> Result<(), String> {
    place_pointer(edge)
}

pub fn pointer_at_edge(edge: Edge) -> Result<bool, String> {
    let point = current_pointer()?;
    let bounds = main_display_bounds();
    Ok(match edge {
        Edge::Left => point.x <= bounds.origin.x + 1.0,
        Edge::Right => point.x >= bounds.origin.x + bounds.size.width - 2.0,
        Edge::Disabled => false,
    })
}

pub fn permission_state() -> PermissionState {
    let capture = unsafe { CGPreflightListenEventAccess() };
    let injection = unsafe { AXIsProcessTrusted() };
    let message = match (capture, injection) {
        (true, true) => "输入监控与辅助功能权限已就绪。",
        (false, false) => "请在系统设置 → 隐私与安全性中开启输入监控和辅助功能。",
        (false, true) => "请在系统设置 → 隐私与安全性中开启输入监控。",
        (true, false) => "请在系统设置 → 隐私与安全性中开启辅助功能。",
    };
    PermissionState {
        capture,
        injection,
        message: message.into(),
    }
}

pub fn request_permissions() -> PermissionState {
    unsafe {
        let _ = CGRequestListenEventAccess();
    }
    if !unsafe { AXIsProcessTrusted() } {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .spawn();
    }
    permission_state()
}

fn main_display_bounds() -> CGRect {
    unsafe { CGDisplayBounds(CGMainDisplayID()) }
}

fn has_emergency_modifiers(modifiers: &HashSet<u16>) -> bool {
    let control = modifiers.contains(&0xE0) || modifiers.contains(&0xE4);
    let shift = modifiers.contains(&0xE1) || modifiers.contains(&0xE5);
    let option = modifiers.contains(&0xE2) || modifiers.contains(&0xE6);
    control && shift && option
}

fn is_modifier(usage: u16) -> bool {
    (0xE0..=0xE7).contains(&usage)
}

fn mac_keycode_to_hid(key: u16) -> Option<u16> {
    Some(match key {
        0 => 0x04,
        11 => 0x05,
        8 => 0x06,
        2 => 0x07,
        14 => 0x08,
        3 => 0x09,
        5 => 0x0A,
        4 => 0x0B,
        34 => 0x0C,
        38 => 0x0D,
        40 => 0x0E,
        37 => 0x0F,
        46 => 0x10,
        45 => 0x11,
        31 => 0x12,
        35 => 0x13,
        12 => 0x14,
        15 => 0x15,
        1 => 0x16,
        17 => 0x17,
        32 => 0x18,
        9 => 0x19,
        13 => 0x1A,
        7 => 0x1B,
        16 => 0x1C,
        6 => 0x1D,
        18 => 0x1E,
        19 => 0x1F,
        20 => 0x20,
        21 => 0x21,
        23 => 0x22,
        22 => 0x23,
        26 => 0x24,
        28 => 0x25,
        25 => 0x26,
        29 => 0x27,
        36 => 0x28,
        53 => 0x29,
        51 => 0x2A,
        48 => 0x2B,
        49 => 0x2C,
        27 => 0x2D,
        24 => 0x2E,
        33 => 0x2F,
        30 => 0x30,
        42 => 0x31,
        41 => 0x33,
        39 => 0x34,
        50 => 0x35,
        43 => 0x36,
        47 => 0x37,
        44 => 0x38,
        57 => 0x39,
        122 => 0x3A,
        120 => 0x3B,
        99 => 0x3C,
        118 => 0x3D,
        96 => 0x3E,
        97 => 0x3F,
        98 => 0x40,
        100 => 0x41,
        101 => 0x42,
        109 => 0x43,
        103 => 0x44,
        111 => 0x45,
        114 => 0x49,
        115 => 0x4A,
        116 => 0x4B,
        117 => 0x4C,
        119 => 0x4D,
        121 => 0x4E,
        124 => 0x4F,
        123 => 0x50,
        125 => 0x51,
        126 => 0x52,
        59 => 0xE0,
        56 => 0xE1,
        58 => 0xE2,
        55 => 0xE3,
        62 => 0xE4,
        60 => 0xE5,
        61 => 0xE6,
        54 => 0xE7,
        _ => return None,
    })
}

fn hid_to_mac_keycode(usage: u16) -> Option<u16> {
    Some(match usage {
        0x04 => 0,
        0x05 => 11,
        0x06 => 8,
        0x07 => 2,
        0x08 => 14,
        0x09 => 3,
        0x0A => 5,
        0x0B => 4,
        0x0C => 34,
        0x0D => 38,
        0x0E => 40,
        0x0F => 37,
        0x10 => 46,
        0x11 => 45,
        0x12 => 31,
        0x13 => 35,
        0x14 => 12,
        0x15 => 15,
        0x16 => 1,
        0x17 => 17,
        0x18 => 32,
        0x19 => 9,
        0x1A => 13,
        0x1B => 7,
        0x1C => 16,
        0x1D => 6,
        0x1E => 18,
        0x1F => 19,
        0x20 => 20,
        0x21 => 21,
        0x22 => 23,
        0x23 => 22,
        0x24 => 26,
        0x25 => 28,
        0x26 => 25,
        0x27 => 29,
        0x28 => 36,
        0x29 => 53,
        0x2A => 51,
        0x2B => 48,
        0x2C => 49,
        0x2D => 27,
        0x2E => 24,
        0x2F => 33,
        0x30 => 30,
        0x31 => 42,
        0x33 => 41,
        0x34 => 39,
        0x35 => 50,
        0x36 => 43,
        0x37 => 47,
        0x38 => 44,
        0x39 => 57,
        0x3A => 122,
        0x3B => 120,
        0x3C => 99,
        0x3D => 118,
        0x3E => 96,
        0x3F => 97,
        0x40 => 98,
        0x41 => 100,
        0x42 => 101,
        0x43 => 109,
        0x44 => 103,
        0x45 => 111,
        0x49 => 114,
        0x4A => 115,
        0x4B => 116,
        0x4C => 117,
        0x4D => 119,
        0x4E => 121,
        0x4F => 124,
        0x50 => 123,
        0x51 => 125,
        0x52 => 126,
        0xE0 => 59,
        0xE1 => 56,
        0xE2 => 58,
        0xE3 => 55,
        0xE4 => 62,
        0xE5 => 60,
        0xE6 => 61,
        0xE7 => 54,
        _ => return None,
    })
}
