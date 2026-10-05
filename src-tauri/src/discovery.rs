use crate::config::{Settings, DISCOVERY_PORT};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DISCOVERY_MAGIC: &str = "deskbridge-discovery-v1";
const PEER_EXPIRY: Duration = Duration::from_secs(6);
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
}

#[derive(Debug, Serialize, Deserialize)]
struct Advertisement {
    magic: String,
    version: u16,
    node_id: String,
    name: String,
    os: String,
    port: u16,
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
                let advertisement = Advertisement {
                    magic: DISCOVERY_MAGIC.into(),
                    version: 1,
                    node_id: settings.node_id.clone(),
                    name: settings.device_name.clone(),
                    os: current_os().into(),
                    port: settings.listen_port,
                };
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
    if settings
        .read()
        .map(|settings| settings.node_id == advertisement.node_id)
        .unwrap_or(true)
    {
        return;
    }

    let peer = PeerInfo {
        node_id: advertisement.node_id.clone(),
        name: advertisement.name,
        os: advertisement.os,
        address: source.ip().to_string(),
        port: advertisement.port,
        last_seen_ms: unix_time_ms(),
        seen_at: Instant::now(),
    };
    if let Ok(mut table) = peers.write() {
        // 同一地址在现有记录过期前不接受其他节点认领，防止伪造广播挤掉真实设备。
        if table.iter().any(|(node_id, existing)| {
            node_id != &advertisement.node_id
                && existing.address == peer.address
                && existing.seen_at.elapsed() < PEER_EXPIRY
        }) {
            return;
        }
        table.retain(|node_id, existing| {
            node_id == &advertisement.node_id || existing.address != peer.address
        });
        if table.contains_key(&advertisement.node_id) || table.len() < MAX_DISCOVERED_PEERS {
            table.insert(advertisement.node_id, peer);
        }
    }
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
}
