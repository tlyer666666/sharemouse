use super::{CaptureControl, CaptureEvent, PermissionState};
use crate::config::Edge;
use std::sync::{mpsc::SyncSender, Arc};
use std::thread::{self, JoinHandle};

pub fn spawn_capture(
    _sender: SyncSender<CaptureEvent>,
    _control: Arc<CaptureControl>,
) -> Result<JoinHandle<()>, String> {
    thread::Builder::new()
        .name("deskbridge-input-stub".into())
        .spawn(|| {})
        .map_err(|error| error.to_string())
}

pub fn inject_mouse_move(_dx: i32, _dy: i32) -> Result<(), String> {
    Err("当前平台不支持输入注入".into())
}

pub fn inject_mouse_button(_button: u8, _down: bool) -> Result<(), String> {
    Err("当前平台不支持输入注入".into())
}

pub fn inject_wheel(_horizontal: i32, _vertical: i32) -> Result<(), String> {
    Err("当前平台不支持输入注入".into())
}

pub fn inject_key(_usage: u16, _down: bool) -> Result<(), String> {
    Err("当前平台不支持输入注入".into())
}

pub fn place_pointer(_edge: Edge) -> Result<(), String> {
    Ok(())
}

pub fn restore_local_pointer(_edge: Edge) -> Result<(), String> {
    Ok(())
}

pub fn pointer_at_edge(_edge: Edge) -> Result<bool, String> {
    Ok(false)
}

pub fn permission_state() -> PermissionState {
    PermissionState {
        capture: false,
        injection: false,
        message: "当前仅支持 Windows 与 macOS".into(),
    }
}

pub fn request_permissions() -> PermissionState {
    permission_state()
}
