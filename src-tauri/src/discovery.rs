use crate::config::{Settings, DISCOVERY_PORT};
use ring::hmac;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DISCOVERY_MAGIC: &str = "deskbridge-discovery-v1";
const DISCOVERY_AUTH_CONTEXT: &[u8] = b"deskbridge-discovery-auth-v1\0";
const PEER_EXPIRY: Duration = Duration::from_secs(6);
const AUTH_TIME_WINDOW_SECS: u64 = 30;
const MAX_DISCOVERED_PEERS: usize = 64;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerInfo {
    pub node_id: String,
    pub name: String,
    pub os: String,
    pub address: String,
    pub port: u16,
    pub last_seen_ms: u64,
    #[serde(skip)]
    seen_at: Instant,
    #[serde(skip)]
    authenticated: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Advertisement {
    magic: String,
    version: u16,
    node_id: String,
    name: String,
    os: String,
    port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timestamp_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth_tag: Option<String>,
}

pub type PeerTable = Arc<RwLock<HashMap<String, PeerInfo>>>;

pub fn spawn(
    settings: Arc<RwLock<Settings>>,
    peers: PeerTable,
    shutdown: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, String> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT))
        .map_err(|error| format!("无法监听局域网发现端口 {DISCOVERY_PORT}：{error}"))?;
    socket
        .set_broadcast(true)
        .map_err(|error| format!("无法启用局域网广播：{error}"))?;
    socket
        .set_read_timeout(Some(Duration::from_millis(250)))
        .map_err(|error| format!("无法配置发现服务：{error}"))?;

    thread::Builder::new()
        .name("deskbridge-discovery".into())
        .spawn(move || discovery_loop(socket, settings, peers, shutdown))
        .map_err(|error| format!("无法启动发现服务：{error}"))
}

fn discovery_loop(
    socket: UdpSocket,
    settings: Arc<RwLock<Settings>>,
    peers: PeerTable,
    shutdown: Arc<AtomicBool>,
) {
    let destination = SocketAddrV4::new(Ipv4Addr::BROADCAST, DISCOVERY_PORT);
    let mut last_broadcast = Instant::now() - Duration::from_secs(2);
    let mut buffer = [0_u8; 1400];

    while !shutdown.load(Ordering::Acquire) {
        if last_broadcast.elapsed() >= Duration::from_secs(1) {
            if let Ok(settings) = settings.read() {
                let mut advertisement = Advertisement {
                    magic: DISCOVERY_MAGIC.into(),
                    version: 1,
                    node_id: settings.node_id.clone(),
                    name: settings.device_name.clone(),
                    os: current_os().into(),
                    port: settings.listen_port,
                    timestamp_sec: None,
                    auth_tag: None,
                };
                if let Ok(key) = settings.pairing_key_bytes() {
                    sign_advertisement(&mut advertisement, &key, unix_time_secs());
                }
                if let Ok(payload) = serde_json::to_vec(&advertisement) {
                    let _ = socket.send_to(&payload, destination);
                }
            }
            last_broadcast = Instant::now();
            expire_old_peers(&peers);
        }

        match socket.recv_from(&mut buffer) {
            Ok((length, source)) => {
                if let Ok(advertisement) =
                    serde_json::from_slice::<Advertisement>(&buffer[..length])
                {
                    accept_advertisement(advertisement, source, &settings, &peers);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => thread::sleep(Duration::from_millis(200)),
        }
    }
}

fn accept_advertisement(
    advertisement: Advertisement,
    source: SocketAddr,
    settings: &Arc<RwLock<Settings>>,
    peers: &PeerTable,
) {
    if advertisement.magic != DISCOVERY_MAGIC
        || advertisement.version != 1
        || advertisement.node_id.len() != 32
        || !advertisement
            .node_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || advertisement.name.is_empty()
        || advertisement.name.chars().count() > 48
        || !matches!(advertisement.os.as_str(), "Windows" | "macOS")
        || advertisement.port < 1024
        || !is_lan_source(source)
    {
        return;
    }
    let (own_node_id, pairing_key) = match settings.read() {
        Ok(settings) => (settings.node_id.clone(), settings.pairing_key_bytes().ok()),
        Err(_) => return,
    };
    if own_node_id == advertisement.node_id {
        return;
    }

    let authenticated = pairing_key
        .as_ref()
        .is_some_and(|key| verify_advertisement_auth(&advertisement, key, unix_time_secs()));

    let peer = PeerInfo {
        node_id: advertisement.node_id.clone(),
        name: advertisement.name,
        os: advertisement.os,
        address: source.ip().to_string(),
        port: advertisement.port,
        last_seen_ms: unix_time_ms(),
        seen_at: Instant::now(),
        authenticated,
    };
    if let Ok(mut table) = peers.write() {
        // 同一地址在现有记录过期前不接受其他节点认领，防止伪造广播挤掉真实设备。
        if table.iter().any(|(node_id, existing)| {
            node_id != &advertisement.node_id
                && existing.address == peer.address
                && existing.seen_at.elapsed() < PEER_EXPIRY
                && (!authenticated || existing.authenticated)
        }) {
            return;
        }
        // 已存在的未过期 node_id 拒绝来自不同 IP 的未认证覆盖，同时拒绝未认证广播降级已认证记录。
        if let Some(existing) = table.get(&advertisement.node_id) {
            if existing.seen_at.elapsed() < PEER_EXPIRY
                && !authenticated
                && (existing.address != peer.address || existing.authenticated)
            {
                return;
            }
        }
        table.retain(|node_id, existing| {
            node_id == &advertisement.node_id || existing.address != peer.address
        });
        if table.contains_key(&advertisement.node_id) || table.len() < MAX_DISCOVERED_PEERS {
            table.insert(advertisement.node_id, peer);
        }
    }
}

fn discovery_auth_payload(
    version: u16,
    node_id: &str,
    name: &str,
    os: &str,
    port: u16,
    timestamp_sec: u64,
) -> Vec<u8> {
    let mut payload = Vec::with_capacity(
        DISCOVERY_AUTH_CONTEXT.len() + 2 + node_id.len() + name.len() + os.len() + 13,
    );
    payload.extend_from_slice(DISCOVERY_AUTH_CONTEXT);
    payload.extend_from_slice(&version.to_be_bytes());
    payload.extend_from_slice(node_id.as_bytes());
    payload.push(0);
    payload.extend_from_slice(name.as_bytes());
    payload.push(0);
    payload.extend_from_slice(os.as_bytes());
    payload.push(0);
    payload.extend_from_slice(&port.to_be_bytes());
    payload.extend_from_slice(&timestamp_sec.to_be_bytes());
    payload
}

fn sign_advertisement(
    advertisement: &mut Advertisement,
    pairing_key: &[u8; 32],
    timestamp_sec: u64,
) {
    let payload = discovery_auth_payload(
        advertisement.version,
        &advertisement.node_id,
        &advertisement.name,
        &advertisement.os,
        advertisement.port,
        timestamp_sec,
    );
    let key = hmac::Key::new(hmac::HMAC_SHA256, pairing_key);
    let tag = hmac::sign(&key, &payload);
    let mut hex = String::with_capacity(64);
    for byte in tag.as_ref() {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    advertisement.timestamp_sec = Some(timestamp_sec);
    advertisement.auth_tag = Some(hex);
}

fn verify_advertisement_auth(
    advertisement: &Advertisement,
    pairing_key: &[u8; 32],
    now_sec: u64,
) -> bool {
    let (Some(timestamp_sec), Some(tag_hex)) = (
        advertisement.timestamp_sec,
        advertisement.auth_tag.as_deref(),
    ) else {
        return false;
    };
    if now_sec.abs_diff(timestamp_sec) > AUTH_TIME_WINDOW_SECS {
        return false;
    }
    let Some(tag_bytes) = decode_hex_32_bytes(tag_hex) else {
        return false;
    };
    let payload = discovery_auth_payload(
        advertisement.version,
        &advertisement.node_id,
        &advertisement.name,
        &advertisement.os,
        advertisement.port,
        timestamp_sec,
    );
    let key = hmac::Key::new(hmac::HMAC_SHA256, pairing_key);
    hmac::verify(&key, &payload, &tag_bytes).is_ok()
}

fn decode_hex_32_bytes(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut output = [0_u8; 32];
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(output)
}

fn unix_time_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn is_lan_source(source: SocketAddr) -> bool {
    match source.ip() {
        std::net::IpAddr::V4(address) => {
            address.is_private() || address.is_loopback() || address.is_link_local()
        }
        std::net::IpAddr::V6(_) => false,
    }
}

fn expire_old_peers(peers: &PeerTable) {
    if let Ok(mut table) = peers.write() {
        table.retain(|_, peer| peer.seen_at.elapsed() < PEER_EXPIRY);
    }
}

pub fn snapshot(peers: &PeerTable) -> Vec<PeerInfo> {
    let mut list = peers
        .read()
        .map(|table| table.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    list.sort_by_key(|peer| peer.name.to_lowercase());
    list
}

fn current_os() -> &'static str {
    #[cfg(target_os = "windows")]
    return "Windows";
    #[cfg(target_os = "macos")]
    return "macOS";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    return std::env::consts::OS;
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advertisement(index: usize) -> Advertisement {
        Advertisement {
            magic: DISCOVERY_MAGIC.into(),
            version: 1,
            node_id: format!("{index:032x}"),
            name: format!("Peer {index}"),
            os: "Windows".into(),
            port: 24_817,
            timestamp_sec: None,
            auth_tag: None,
        }
    }

    #[test]
    fn discovered_peer_table_is_bounded() {
        let settings = Arc::new(RwLock::new(Settings::default()));
        let peers = Arc::new(RwLock::new(HashMap::new()));
        for index in 1..=80 {
            let source = format!("10.0.{}.{}:24816", index / 250, index % 250 + 1)
                .parse()
                .unwrap();
            accept_advertisement(advertisement(index), source, &settings, &peers);
        }
        assert_eq!(peers.read().unwrap().len(), MAX_DISCOVERED_PEERS);
    }

    #[test]
    fn public_discovery_sources_are_rejected() {
        let settings = Arc::new(RwLock::new(Settings::default()));
        let peers = Arc::new(RwLock::new(HashMap::new()));
        accept_advertisement(
            advertisement(1),
            "8.8.8.8:24816".parse().unwrap(),
            &settings,
            &peers,
        );
        assert!(peers.read().unwrap().is_empty());
    }

    #[test]
    fn fresh_address_binding_rejects_other_node_ids() {
        let settings = Arc::new(RwLock::new(Settings::default()));
        let peers = Arc::new(RwLock::new(HashMap::new()));
        let source = "10.0.0.5:24816".parse().unwrap();
        accept_advertisement(advertisement(1), source, &settings, &peers);
        accept_advertisement(advertisement(2), source, &settings, &peers);
        let table = peers.read().unwrap();
        assert_eq!(table.len(), 1);
        assert!(table.contains_key(&format!("{:032x}", 1usize)));
        drop(table);

        let backdated = Instant::now()
            .checked_sub(PEER_EXPIRY + Duration::from_secs(1))
            .unwrap();
        if let Ok(mut table) = peers.write() {
            for peer in table.values_mut() {
                peer.seen_at = backdated;
            }
        }
        accept_advertisement(advertisement(2), source, &settings, &peers);
        let table = peers.read().unwrap();
        assert_eq!(table.len(), 1);
        assert!(table.contains_key(&format!("{:032x}", 2usize)));
    }

    #[test]
    fn unauthenticated_different_ip_cannot_overwrite_fresh_node_id() {
        let settings = Arc::new(RwLock::new(Settings::default()));
        let peers = Arc::new(RwLock::new(HashMap::new()));
        let real_source = "10.0.0.5:24816".parse().unwrap();
        let spoof_source = "10.0.0.99:24816".parse().unwrap();

        accept_advertisement(advertisement(1), real_source, &settings, &peers);
        accept_advertisement(advertisement(1), spoof_source, &settings, &peers);

        let table = peers.read().unwrap();
        let peer = table.get(&format!("{:032x}", 1usize)).unwrap();
        assert_eq!(peer.address, "10.0.0.5");
    }

    #[test]
    fn authenticated_peer_updates_ip_and_rejects_bad_or_stale_hmac() {
        let settings_val = Settings::default();
        let key = settings_val.pairing_key_bytes().unwrap();
        let settings = Arc::new(RwLock::new(settings_val));
        let peers = Arc::new(RwLock::new(HashMap::new()));

        let mut initial = advertisement(1);
        sign_advertisement(&mut initial, &key, unix_time_secs());
        accept_advertisement(
            initial,
            "10.0.0.5:24816".parse().unwrap(),
            &settings,
            &peers,
        );

        // Stale timestamp must not authenticate or overwrite IP.
        let mut stale = advertisement(1);
        sign_advertisement(
            &mut stale,
            &key,
            unix_time_secs().saturating_sub(AUTH_TIME_WINDOW_SECS + 10),
        );
        accept_advertisement(stale, "10.0.0.6:24816".parse().unwrap(), &settings, &peers);
        assert_eq!(
            peers
                .read()
                .unwrap()
                .get(&format!("{:032x}", 1usize))
                .unwrap()
                .address,
            "10.0.0.5"
        );

        // Wrong key must not authenticate or overwrite IP.
        let mut wrong_key_adv = advertisement(1);
        sign_advertisement(&mut wrong_key_adv, &[0xAB; 32], unix_time_secs());
        accept_advertisement(
            wrong_key_adv,
            "10.0.0.7:24816".parse().unwrap(),
            &settings,
            &peers,
        );
        assert_eq!(
            peers
                .read()
                .unwrap()
                .get(&format!("{:032x}", 1usize))
                .unwrap()
                .address,
            "10.0.0.5"
        );

        // Valid fresh HMAC from paired peer legitimately updates IP.
        let mut moved = advertisement(1);
        sign_advertisement(&mut moved, &key, unix_time_secs());
        accept_advertisement(moved, "10.0.0.8:24816".parse().unwrap(), &settings, &peers);
        assert_eq!(
            peers
                .read()
                .unwrap()
                .get(&format!("{:032x}", 1usize))
                .unwrap()
                .address,
            "10.0.0.8"
        );
    }
}
