use super::{try_send_capture, CaptureControl, CaptureEvent, PermissionState};
use crate::config::Edge;
use crate::protocol::WireMessage;
use std::collections::HashSet;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::null_mut;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Instant;

type Hhook = *mut c_void;
type Hinstance = *mut c_void;
type Hwnd = *mut c_void;

const WH_KEYBOARD_LL: i32 = 13;
const WH_MOUSE_LL: i32 = 14;
const HC_ACTION: i32 = 0;
const WM_KEYDOWN: usize = 0x0100;
const WM_KEYUP: usize = 0x0101;
const WM_SYSKEYDOWN: usize = 0x0104;
const WM_SYSKEYUP: usize = 0x0105;
const WM_MOUSEMOVE: usize = 0x0200;
const WM_LBUTTONDOWN: usize = 0x0201;
const WM_LBUTTONUP: usize = 0x0202;
const WM_RBUTTONDOWN: usize = 0x0204;
const WM_RBUTTONUP: usize = 0x0205;
const WM_MBUTTONDOWN: usize = 0x0207;
const WM_MBUTTONUP: usize = 0x0208;
const WM_MOUSEWHEEL: usize = 0x020A;
const WM_XBUTTONDOWN: usize = 0x020B;
const WM_XBUTTONUP: usize = 0x020C;
const WM_MOUSEHWHEEL: usize = 0x020E;
const LLMHF_INJECTED: u32 = 0x0000_0001;
const LLKHF_INJECTED: u32 = 0x0000_0010;
const VK_ESCAPE: u32 = 0x1B;
const SM_XVIRTUALSCREEN: i32 = 76;
const SM_YVIRTUALSCREEN: i32 = 77;
const SM_CXVIRTUALSCREEN: i32 = 78;
const SM_CYVIRTUALSCREEN: i32 = 79;
const INPUT_MOUSE: u32 = 0;
const INPUT_KEYBOARD: u32 = 1;
const KEYEVENTF_EXTENDEDKEY: u32 = 0x0001;
const KEYEVENTF_KEYUP: u32 = 0x0002;
const KEYEVENTF_SCANCODE: u32 = 0x0008;
const MOUSEEVENTF_MOVE: u32 = 0x0001;
const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
const MOUSEEVENTF_RIGHTDOWN: u32 = 0x0008;
const MOUSEEVENTF_RIGHTUP: u32 = 0x0010;
const MOUSEEVENTF_MIDDLEDOWN: u32 = 0x0020;
const MOUSEEVENTF_MIDDLEUP: u32 = 0x0040;
const MOUSEEVENTF_XDOWN: u32 = 0x0080;
const MOUSEEVENTF_XUP: u32 = 0x0100;
const MOUSEEVENTF_WHEEL: u32 = 0x0800;
const MOUSEEVENTF_HWHEEL: u32 = 0x1000;
const MOUSEEVENTF_VIRTUALDESK: u32 = 0x4000;
const MOUSEEVENTF_ABSOLUTE: u32 = 0x8000;
const MAPVK_VK_TO_VSC_EX: u32 = 4;
const INJECTION_SENTINEL: usize = 0x4453_4B42;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
struct Msg {
    hwnd: Hwnd,
    message: u32,
    w_param: usize,
    l_param: isize,
    time: u32,
    point: Point,
}

#[repr(C)]
struct MouseHookData {
    point: Point,
    mouse_data: u32,
    flags: u32,
    time: u32,
    extra_info: usize,
}

#[repr(C)]
struct KeyboardHookData {
    virtual_key: u32,
    scan_code: u32,
    flags: u32,
    time: u32,
    extra_info: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MouseInput {
    dx: i32,
    dy: i32,
    mouse_data: u32,
    flags: u32,
    time: u32,
    extra_info: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KeyboardInput {
    virtual_key: u16,
    scan_code: u16,
    flags: u32,
    time: u32,
    extra_info: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct HardwareInput {
    message: u32,
    parameter_low: u16,
    parameter_high: u16,
}

#[repr(C)]
union InputUnion {
    mouse: MouseInput,
    keyboard: KeyboardInput,
    hardware: HardwareInput,
}

#[repr(C)]
struct Input {
    kind: u32,
    value: InputUnion,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn SetWindowsHookExW(
        id_hook: i32,
        callback: Option<unsafe extern "system" fn(i32, usize, isize) -> isize>,
        module: Hinstance,
        thread_id: u32,
    ) -> Hhook;
    fn UnhookWindowsHookEx(hook: Hhook) -> i32;
    fn CallNextHookEx(hook: Hhook, code: i32, w_param: usize, l_param: isize) -> isize;
    fn GetMessageW(message: *mut Msg, window: Hwnd, min: u32, max: u32) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn GetCursorPos(point: *mut Point) -> i32;
    fn GetAsyncKeyState(virtual_key: i32) -> i16;
    fn SetCursorPos(x: i32, y: i32) -> i32;
    fn SendInput(count: u32, inputs: *const Input, size: i32) -> u32;
    fn MapVirtualKeyW(code: u32, map_type: u32) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(module_name: *const u16) -> Hinstance;
    fn GetLastError() -> u32;
}

struct HookContext {
    sender: SyncSender<CaptureEvent>,
    control: Arc<CaptureControl>,
    state: Mutex<HookState>,
}

#[derive(Default)]
struct HookState {
    anchor: Point,
    was_active: bool,
    edge_since: Option<Instant>,
    edge_latched: bool,
    pressed_buttons: HashSet<u8>,
    pressed_keys: HashSet<u16>,
    pressed_modifiers: HashSet<u16>,
}

static CONTEXT: OnceLock<HookContext> = OnceLock::new();

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

    let (startup_sender, startup_receiver) = mpsc::sync_channel(1);
    let thread = thread::Builder::new()
        .name("deskbridge-win32-hooks".into())
        .spawn(move || hook_thread(startup_sender))
        .map_err(|error| format!("无法启动 Windows 输入捕获：{error}"))?;
    match startup_receiver.recv() {
        Ok(Ok(())) => Ok(thread),
        Ok(Err(error)) => {
            let _ = thread.join();
            Err(error)
        }
        Err(_) => {
            let _ = thread.join();
            Err("Windows 输入捕获线程意外退出".into())
        }
    }
}

fn hook_thread(startup: SyncSender<Result<(), String>>) {
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        if module.is_null() {
            let _ = startup.send(Err(format!(
                "无法读取 Windows 模块句柄（错误 {}）",
                GetLastError()
            )));
            return;
        }
        let mouse_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module, 0);
        let keyboard_hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), module, 0);
        if mouse_hook.is_null() || keyboard_hook.is_null() {
            let error_code = GetLastError();
            if !mouse_hook.is_null() {
                UnhookWindowsHookEx(mouse_hook);
            }
            if !keyboard_hook.is_null() {
                UnhookWindowsHookEx(keyboard_hook);
            }
            let _ = startup.send(Err(format!(
                "无法安装 Windows 输入钩子（错误 {error_code}）"
            )));
            return;
        }
        let _ = startup.send(Ok(()));

        let mut message: Msg = zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {}
        UnhookWindowsHookEx(mouse_hook);
        UnhookWindowsHookEx(keyboard_hook);
    }
}

unsafe extern "system" fn mouse_hook(code: i32, w_param: usize, l_param: isize) -> isize {
    if code != HC_ACTION {
        return CallNextHookEx(null_mut(), code, w_param, l_param);
    }
    let Some(context) = CONTEXT.get() else {
        return CallNextHookEx(null_mut(), code, w_param, l_param);
    };
    let data = &*(l_param as *const MouseHookData);
    if data.flags & LLMHF_INJECTED != 0 || data.extra_info == INJECTION_SENTINEL {
        return CallNextHookEx(null_mut(), code, w_param, l_param);
    }
    let Ok(_transition) = context.control.transition.lock() else {
        return CallNextHookEx(null_mut(), code, w_param, l_param);
    };

    let mut state = match context.state.lock() {
        Ok(state) => state,
        Err(_) => return CallNextHookEx(null_mut(), code, w_param, l_param),
    };
    let button_changed = update_button_state(&mut state, w_param, data.mouse_data);

    let enabled = context.control.enabled.load(Ordering::Acquire);
    let active = context.control.active.load(Ordering::Acquire);
    if !enabled {
        reset_edge_state(&mut state);
        return CallNextHookEx(null_mut(), code, w_param, l_param);
    }

    if active {
        if !state.was_active {
            state.was_active = true;
            let (left, top, width, height) = virtual_screen();
            let centered = SetCursorPos(left + width / 2, top + height / 2) != 0
                && GetCursorPos(&mut state.anchor) != 0;
            if !centered {
                state.anchor = data.point;
            }
            if centered && w_param == WM_MOUSEMOVE {
                return 1;
            }
        }
        if let Some(message) = mouse_message(w_param, data, state.anchor) {
            send_event(context, CaptureEvent::Input(message));
        }
        if w_param == WM_MOUSEMOVE {
            let _ = SetCursorPos(state.anchor.x, state.anchor.y);
        }
        return 1;
    }

    if state.was_active {
        state.was_active = false;
    }
    if context.control.pending.load(Ordering::Acquire) {
        if button_changed || (w_param == WM_MOUSEMOVE && !on_activation_edge(context, data.point)) {
            context.control.pending.store(false, Ordering::Release);
            send_event(context, CaptureEvent::EmergencyRelease);
        }
        return CallNextHookEx(null_mut(), code, w_param, l_param);
    }
    if w_param == WM_MOUSEMOVE && state.pressed_buttons.is_empty() && state.pressed_keys.is_empty()
    {
        watch_edge(context, &mut state, data.point);
    }
    CallNextHookEx(null_mut(), code, w_param, l_param)
}

unsafe extern "system" fn keyboard_hook(code: i32, w_param: usize, l_param: isize) -> isize {
    if code != HC_ACTION {
        return CallNextHookEx(null_mut(), code, w_param, l_param);
    }
    let Some(context) = CONTEXT.get() else {
        return CallNextHookEx(null_mut(), code, w_param, l_param);
    };
    let data = &*(l_param as *const KeyboardHookData);
    if data.flags & LLKHF_INJECTED != 0 || data.extra_info == INJECTION_SENTINEL {
        return CallNextHookEx(null_mut(), code, w_param, l_param);
    }
    let down = w_param == WM_KEYDOWN || w_param == WM_SYSKEYDOWN;
    let up = w_param == WM_KEYUP || w_param == WM_SYSKEYUP;
    if !down && !up {
        return CallNextHookEx(null_mut(), code, w_param, l_param);
    }
    let Ok(_transition) = context.control.transition.lock() else {
        return CallNextHookEx(null_mut(), code, w_param, l_param);
    };
    let usage = virtual_key_to_hid(data.virtual_key);
    let emergency = if let (Some(usage), Ok(mut state)) = (usage, context.state.lock()) {
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
        down && data.virtual_key == VK_ESCAPE && has_emergency_modifiers(&state.pressed_modifiers)
    } else {
        false
    };

    let active = context.control.enabled.load(Ordering::Acquire)
        && context.control.active.load(Ordering::Acquire);
    if !active {
        if context.control.pending.swap(false, Ordering::AcqRel) {
            send_event(context, CaptureEvent::EmergencyRelease);
        }
        return CallNextHookEx(null_mut(), code, w_param, l_param);
    }
    if emergency {
        context.control.active.store(false, Ordering::Release);
        context.control.pending.store(false, Ordering::Release);
        send_event(context, CaptureEvent::EmergencyRelease);
        return 1;
    }
    let Some(usage) = usage else {
        return 1;
    };
    send_event(
        context,
        CaptureEvent::Input(WireMessage::Key { usage, down }),
    );
    1
}

fn send_event(context: &HookContext, event: CaptureEvent) {
    try_send_capture(&context.sender, &context.control, event);
}

fn watch_edge(context: &HookContext, state: &mut HookState, point: Point) {
    let edge = Edge::from_u8(context.control.edge.load(Ordering::Acquire));
    if edge == Edge::Disabled {
        reset_edge_state(state);
        return;
    }
    let (left, top, width, height) = virtual_screen();
    let at_edge = match edge {
        Edge::Left => point.x <= left + 1,
        Edge::Right => point.x >= left + width - 2,
        Edge::Disabled => false,
    };
    let outside_corner_guard = point.y > top + 24 && point.y < top + height - 24;
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
        reset_edge_state(state);
    }
}

fn physical_input_down() -> bool {
    (1..=255).any(|virtual_key| unsafe { GetAsyncKeyState(virtual_key) } < 0)
}

fn reset_edge_state(state: &mut HookState) {
    state.edge_since = None;
    state.edge_latched = false;
    state.was_active = false;
}

fn update_button_state(state: &mut HookState, message: usize, mouse_data: u32) -> bool {
    let update = match message {
        WM_LBUTTONDOWN => Some((1, true)),
        WM_LBUTTONUP => Some((1, false)),
        WM_RBUTTONDOWN => Some((2, true)),
        WM_RBUTTONUP => Some((2, false)),
        WM_MBUTTONDOWN => Some((3, true)),
        WM_MBUTTONUP => Some((3, false)),
        WM_XBUTTONDOWN => Some((x_button(mouse_data), true)),
        WM_XBUTTONUP => Some((x_button(mouse_data), false)),
        _ => None,
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

fn on_activation_edge(context: &HookContext, point: Point) -> bool {
    let edge = Edge::from_u8(context.control.edge.load(Ordering::Acquire));
    let (left, top, width, height) = virtual_screen();
    let at_edge = match edge {
        Edge::Left => point.x <= left + 1,
        Edge::Right => point.x >= left + width - 2,
        Edge::Disabled => false,
    };
    at_edge && point.y > top + 24 && point.y < top + height - 24
}

fn mouse_message(message: usize, data: &MouseHookData, anchor: Point) -> Option<WireMessage> {
    match message {
        WM_MOUSEMOVE => {
            let dx = data.point.x - anchor.x;
            let dy = data.point.y - anchor.y;
            (dx != 0 || dy != 0).then_some(WireMessage::MouseMove { dx, dy })
        }
        WM_LBUTTONDOWN => Some(WireMessage::MouseButton {
            button: 1,
            down: true,
        }),
        WM_LBUTTONUP => Some(WireMessage::MouseButton {
            button: 1,
            down: false,
        }),
        WM_RBUTTONDOWN => Some(WireMessage::MouseButton {
            button: 2,
            down: true,
        }),
        WM_RBUTTONUP => Some(WireMessage::MouseButton {
            button: 2,
            down: false,
        }),
        WM_MBUTTONDOWN => Some(WireMessage::MouseButton {
            button: 3,
            down: true,
        }),
        WM_MBUTTONUP => Some(WireMessage::MouseButton {
            button: 3,
            down: false,
        }),
        WM_XBUTTONDOWN => Some(WireMessage::MouseButton {
            button: x_button(data.mouse_data),
            down: true,
        }),
        WM_XBUTTONUP => Some(WireMessage::MouseButton {
            button: x_button(data.mouse_data),
            down: false,
        }),
        WM_MOUSEWHEEL => Some(WireMessage::Wheel {
            horizontal: 0,
            vertical: wheel_delta(data.mouse_data),
        }),
        WM_MOUSEHWHEEL => Some(WireMessage::Wheel {
            horizontal: wheel_delta(data.mouse_data),
            vertical: 0,
        }),
        _ => None,
    }
}

fn x_button(mouse_data: u32) -> u8 {
    if (mouse_data >> 16) & 0xffff == 1 {
        4
    } else {
        5
    }
}

fn wheel_delta(mouse_data: u32) -> i32 {
    ((mouse_data >> 16) as u16 as i16) as i32
}

pub fn inject_mouse_move(dx: i32, dy: i32) -> Result<(), String> {
    let mut point = Point::default();
    if unsafe { GetCursorPos(&mut point) } == 0 {
        return Err("无法读取鼠标位置".into());
    }
    let (left, top, width, height) = virtual_screen();
    let target_x = (point.x + dx).clamp(left, left + width - 1);
    let target_y = (point.y + dy).clamp(top, top + height - 1);
    let normalized_x = ((target_x - left) as i64 * 65_535 / i64::from((width - 1).max(1))) as i32;
    let normalized_y = ((target_y - top) as i64 * 65_535 / i64::from((height - 1).max(1))) as i32;
    send_mouse_input(
        normalized_x,
        normalized_y,
        0,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
    )
}

pub fn inject_mouse_button(button: u8, down: bool) -> Result<(), String> {
    let (flags, data) = match (button, down) {
        (1, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (1, false) => (MOUSEEVENTF_LEFTUP, 0),
        (2, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (2, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (3, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (3, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (4, true) => (MOUSEEVENTF_XDOWN, 1),
        (4, false) => (MOUSEEVENTF_XUP, 1),
        (5, true) => (MOUSEEVENTF_XDOWN, 2),
        (5, false) => (MOUSEEVENTF_XUP, 2),
        _ => return Ok(()),
    };
    send_mouse_input(0, 0, data, flags)
}

pub fn inject_wheel(horizontal: i32, vertical: i32) -> Result<(), String> {
    if vertical != 0 {
        send_mouse_input(0, 0, vertical as u32, MOUSEEVENTF_WHEEL)?;
    }
    if horizontal != 0 {
        send_mouse_input(0, 0, horizontal as u32, MOUSEEVENTF_HWHEEL)?;
    }
    Ok(())
}

pub fn inject_key(usage: u16, down: bool) -> Result<(), String> {
    let Some(virtual_key) = hid_to_virtual_key(usage) else {
        return Ok(());
    };
    let mapped = unsafe { MapVirtualKeyW(virtual_key, MAPVK_VK_TO_VSC_EX) };
    if mapped == 0 {
        return Ok(());
    }
    let mut flags = KEYEVENTF_SCANCODE;
    if mapped & 0xff00 != 0 {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = Input {
        kind: INPUT_KEYBOARD,
        value: InputUnion {
            keyboard: KeyboardInput {
                virtual_key: 0,
                scan_code: (mapped & 0xff) as u16,
                flags,
                time: 0,
                extra_info: INJECTION_SENTINEL,
            },
        },
    };
    send_input(&input)
}

fn send_mouse_input(dx: i32, dy: i32, mouse_data: u32, flags: u32) -> Result<(), String> {
    let input = Input {
        kind: INPUT_MOUSE,
        value: InputUnion {
            mouse: MouseInput {
                dx,
                dy,
                mouse_data,
                flags,
                time: 0,
                extra_info: INJECTION_SENTINEL,
            },
        },
    };
    send_input(&input)
}

fn send_input(input: &Input) -> Result<(), String> {
    let sent = unsafe { SendInput(1, input, size_of::<Input>() as i32) };
    if sent == 1 {
        Ok(())
    } else {
        Err("Windows 拒绝输入注入；管理员窗口和 UAC 界面不受支持".into())
    }
}

pub fn place_pointer(edge: Edge) -> Result<(), String> {
    let (left, top, width, height) = virtual_screen();
    let mut current = Point::default();
    unsafe { GetCursorPos(&mut current) };
    let y = current.y.clamp(top + 24, top + height - 25);
    let x = match edge {
        Edge::Left => left + 8,
        Edge::Right => left + width - 9,
        Edge::Disabled => left + width / 2,
    };
    if unsafe { SetCursorPos(x, y) } != 0 {
        Ok(())
    } else {
        Err("无法定位鼠标".into())
    }
}

pub fn restore_local_pointer(edge: Edge) -> Result<(), String> {
    place_pointer(edge)
}

pub fn pointer_at_edge(edge: Edge) -> Result<bool, String> {
    let mut point = Point::default();
    if unsafe { GetCursorPos(&mut point) } == 0 {
        return Err("无法读取鼠标位置".into());
    }
    let (left, _, width, _) = virtual_screen();
    Ok(match edge {
        Edge::Left => point.x <= left + 1,
        Edge::Right => point.x >= left + width - 2,
        Edge::Disabled => false,
    })
}

pub fn permission_state() -> PermissionState {
    PermissionState {
        capture: true,
        injection: true,
        message: "Windows 普通桌面可用；管理员窗口、UAC、锁屏和登录界面不受支持。".into(),
    }
}

pub fn request_permissions() -> PermissionState {
    permission_state()
}

fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
        )
    }
}

fn has_emergency_modifiers(modifiers: &HashSet<u16>) -> bool {
    let control = modifiers.contains(&0xE0) || modifiers.contains(&0xE4);
    let shift = modifiers.contains(&0xE1) || modifiers.contains(&0xE5);
    let alt = modifiers.contains(&0xE2) || modifiers.contains(&0xE6);
    control && shift && alt
}

fn is_modifier(usage: u16) -> bool {
    (0xE0..=0xE7).contains(&usage)
}

fn virtual_key_to_hid(key: u32) -> Option<u16> {
    match key {
        0x41..=0x5A => Some(0x04 + (key - 0x41) as u16),
        0x31..=0x39 => Some(0x1E + (key - 0x31) as u16),
        0x30 => Some(0x27),
        0x0D => Some(0x28),
        0x1B => Some(0x29),
        0x08 => Some(0x2A),
        0x09 => Some(0x2B),
        0x20 => Some(0x2C),
        0xBD => Some(0x2D),
        0xBB => Some(0x2E),
        0xDB => Some(0x2F),
        0xDD => Some(0x30),
        0xDC => Some(0x31),
        0xBA => Some(0x33),
        0xDE => Some(0x34),
        0xC0 => Some(0x35),
        0xBC => Some(0x36),
        0xBE => Some(0x37),
        0xBF => Some(0x38),
        0x14 => Some(0x39),
        0x70..=0x7B => Some(0x3A + (key - 0x70) as u16),
        0x2C => Some(0x46),
        0x91 => Some(0x47),
        0x13 => Some(0x48),
        0x2D => Some(0x49),
        0x24 => Some(0x4A),
        0x21 => Some(0x4B),
        0x2E => Some(0x4C),
        0x23 => Some(0x4D),
        0x22 => Some(0x4E),
        0x27 => Some(0x4F),
        0x25 => Some(0x50),
        0x28 => Some(0x51),
        0x26 => Some(0x52),
        0x90 => Some(0x53),
        0x6F => Some(0x54),
        0x6A => Some(0x55),
        0x6D => Some(0x56),
        0x6B => Some(0x57),
        0x0C => Some(0x58),
        0x61..=0x69 => Some(0x59 + (key - 0x61) as u16),
        0x60 => Some(0x62),
        0x6E => Some(0x63),
        0xA2 | 0x11 => Some(0xE0),
        0xA0 | 0x10 => Some(0xE1),
        0xA4 | 0x12 => Some(0xE2),
        0x5B => Some(0xE3),
        0xA3 => Some(0xE4),
        0xA1 => Some(0xE5),
        0xA5 => Some(0xE6),
        0x5C => Some(0xE7),
        _ => None,
    }
}

fn hid_to_virtual_key(usage: u16) -> Option<u32> {
    match usage {
        0x04..=0x1D => Some(0x41 + u32::from(usage - 0x04)),
        0x1E..=0x26 => Some(0x31 + u32::from(usage - 0x1E)),
        0x27 => Some(0x30),
        0x28 => Some(0x0D),
        0x29 => Some(0x1B),
        0x2A => Some(0x08),
        0x2B => Some(0x09),
        0x2C => Some(0x20),
        0x2D => Some(0xBD),
        0x2E => Some(0xBB),
        0x2F => Some(0xDB),
        0x30 => Some(0xDD),
        0x31 => Some(0xDC),
        0x33 => Some(0xBA),
        0x34 => Some(0xDE),
        0x35 => Some(0xC0),
        0x36 => Some(0xBC),
        0x37 => Some(0xBE),
        0x38 => Some(0xBF),
        0x39 => Some(0x14),
        0x3A..=0x45 => Some(0x70 + u32::from(usage - 0x3A)),
        0x46 => Some(0x2C),
        0x47 => Some(0x91),
        0x48 => Some(0x13),
        0x49 => Some(0x2D),
        0x4A => Some(0x24),
        0x4B => Some(0x21),
        0x4C => Some(0x2E),
        0x4D => Some(0x23),
        0x4E => Some(0x22),
        0x4F => Some(0x27),
        0x50 => Some(0x25),
        0x51 => Some(0x28),
        0x52 => Some(0x26),
        0x53 => Some(0x90),
        0x54 => Some(0x6F),
        0x55 => Some(0x6A),
        0x56 => Some(0x6D),
        0x57 => Some(0x6B),
        0x58 => Some(0x0C),
        0x59..=0x61 => Some(0x61 + u32::from(usage - 0x59)),
        0x62 => Some(0x60),
        0x63 => Some(0x6E),
        0xE0 => Some(0xA2),
        0xE1 => Some(0xA0),
        0xE2 => Some(0xA4),
        0xE3 => Some(0x5B),
        0xE4 => Some(0xA3),
        0xE5 => Some(0xA1),
        0xE6 => Some(0xA5),
        0xE7 => Some(0x5C),
        _ => None,
    }
}
