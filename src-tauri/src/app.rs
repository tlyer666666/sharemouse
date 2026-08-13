use crate::config::{self, Edge, Settings};
use crate::discovery::PeerInfo;
use crate::network::{RuntimeStatus, Service};
use crate::platform::{self, PermissionState};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tauri::State;

pub struct AppContext {
    service: Arc<Service>,
    config_path: PathBuf,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    settings: Settings,
    pairing_key_display: String,
    peers: Vec<PeerInfo>,
    status: RuntimeStatus,
    permissions: PermissionState,
    platform: String,
    version: &'static str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsUpdate {
    device_name: String,
    pairing_key: String,
    selected_peer_id: Option<String>,
    manual_address: String,
    peer_edge: Edge,
    edge_delay_ms: u64,
    enabled: bool,
}

#[tauri::command]
fn get_snapshot(context: State<'_, AppContext>) -> Result<Snapshot, String> {
    let settings = read_clone(&context.service.settings)?;
    let status = read_clone(&context.service.status)?;
    Ok(Snapshot {
        pairing_key_display: config::format_pairing_key(&settings.pairing_key),
        settings,
        peers: context.service.peer_snapshot(),
        status,
        permissions: platform::permission_state(),
        platform: std::env::consts::OS.into(),
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[tauri::command]
fn update_settings(
    update: SettingsUpdate,
    context: State<'_, AppContext>,
) -> Result<Snapshot, String> {
    let mut settings = read_clone(&context.service.settings)?;
    let previous_pairing_key = settings.pairing_key.clone();
    let previous_peer_id = settings.selected_peer_id.clone();
    let previous_manual_address = settings.manual_address.clone();

    settings.device_name = update.device_name;
    settings.pairing_key = update.pairing_key;
    settings.selected_peer_id = update
        .selected_peer_id
        .filter(|peer_id| !peer_id.trim().is_empty());
    settings.manual_address = update.manual_address;
    settings.peer_edge = update.peer_edge;
    settings.edge_delay_ms = update.edge_delay_ms;
    settings.enabled = update.enabled;
    settings.validate()?;
    if settings.enabled {
        if settings.selected_peer_id.is_none() && settings.manual_address.is_empty() {
            return Err("请先选择另一台电脑，或填写手动 IP".into());
        }
        let permissions = platform::permission_state();
        if !permissions.capture || !permissions.injection {
            return Err("请先授予鼠标键盘控制权限".into());
        }
    }
    let connection_sensitive_change = previous_pairing_key != settings.pairing_key
        || previous_peer_id != settings.selected_peer_id
        || previous_manual_address != settings.manual_address;
    config::save_to(&context.config_path, &settings)?;
    {
        let mut live = context
            .service
            .settings
            .write()
            .map_err(|_| "配置状态不可用".to_string())?;
        *live = settings.clone();
    }
    if connection_sensitive_change {
        context.service.release_control();
    }
    context.service.update_capture_settings(&settings);
    get_snapshot(context)
}

#[tauri::command]
fn generate_pairing_key(context: State<'_, AppContext>) -> Result<Snapshot, String> {
    let current = read_clone(&context.service.settings)?;
    update_settings(
        SettingsUpdate {
            device_name: current.device_name,
            pairing_key: config::random_pairing_key(),
            selected_peer_id: current.selected_peer_id,
            manual_address: current.manual_address,
            peer_edge: current.peer_edge,
            edge_delay_ms: current.edge_delay_ms,
            enabled: false,
        },
        context,
    )
}

#[tauri::command]
fn return_control(context: State<'_, AppContext>) -> Result<Snapshot, String> {
    context.service.release_control();
    std::thread::sleep(std::time::Duration::from_millis(30));
    get_snapshot(context)
}

#[tauri::command]
fn request_permissions(context: State<'_, AppContext>) -> Result<Snapshot, String> {
    let _ = platform::request_permissions();
    get_snapshot(context)
}

pub fn run() {
    let (settings, config_path) = config::load_or_create().unwrap_or_else(|error| {
        eprintln!("DeskBridge configuration warning: {error}");
        let settings = Settings::default();
        let fallback = std::env::temp_dir().join("deskbridge-settings.json");
        (settings, fallback)
    });
    let service = Service::start(settings).unwrap_or_else(|error| {
        panic!("DeskBridge 无法启动：{error}");
    });

    tauri::Builder::default()
        .manage(AppContext {
            service,
            config_path,
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            update_settings,
            generate_pairing_key,
            return_control,
            request_permissions
        ])
        .run(tauri::generate_context!())
        .expect("DeskBridge runtime failed");
}

fn read_clone<T: Clone>(lock: &RwLock<T>) -> Result<T, String> {
    lock.read()
        .map(|value| value.clone())
        .map_err(|_| "应用状态不可用".to_string())
}
