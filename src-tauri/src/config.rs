use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const DISCOVERY_PORT: u16 = 24_816;
pub const DEFAULT_CONTROL_PORT: u16 = 24_817;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Left,
    Right,
    Disabled,
}

impl Edge {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Disabled => 0,
            Self::Left => 1,
            Self::Right => 2,
        }
    }

    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Left,
            2 => Self::Right,
            _ => Self::Disabled,
        }
    }

    pub fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Disabled => Self::Disabled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub node_id: String,
    pub device_name: String,
    pub pairing_key: String,
    pub listen_port: u16,
    pub selected_peer_id: Option<String>,
    pub manual_address: String,
    pub peer_edge: Edge,
    pub edge_delay_ms: u64,
    pub enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            node_id: random_hex(16),
            device_name: default_device_name(),
            pairing_key: random_hex(32),
            listen_port: DEFAULT_CONTROL_PORT,
            selected_peer_id: None,
            manual_address: String::new(),
            peer_edge: Edge::Right,
            edge_delay_ms: 180,
            enabled: false,
        }
    }
}

impl Settings {
    pub fn validate(&mut self) -> Result<(), String> {
        self.device_name = self.device_name.trim().to_string();
        if self.device_name.is_empty() || self.device_name.chars().count() > 48 {
            return Err("设备名称需要为 1–48 个字符".into());
        }

        self.pairing_key = normalize_pairing_key(&self.pairing_key)?;
        if self.listen_port < 1024 {
            return Err("监听端口需要在 1024–65535 之间".into());
        }
        if !(80..=1500).contains(&self.edge_delay_ms) {
            return Err("边缘触发时间需要在 80–1500 毫秒之间".into());
        }

        self.manual_address = self.manual_address.trim().to_string();
        if !self.manual_address.is_empty() {
            validate_manual_address(&self.manual_address)?;
        }
        Ok(())
    }

    pub fn pairing_key_bytes(&self) -> Result<[u8; 32], String> {
        decode_hex_32(&self.pairing_key)
    }
}

pub fn settings_path() -> Result<PathBuf, String> {
    let base = dirs::config_dir().ok_or_else(|| "找不到系统配置目录".to_string())?;
    Ok(base.join("DeskBridge").join("settings.json"))
}

pub fn load_or_create() -> Result<(Settings, PathBuf), String> {
    let path = settings_path()?;
    if path.exists() {
        let data = fs::read_to_string(&path).map_err(|error| format!("读取配置失败：{error}"))?;
        let mut settings: Settings =
            serde_json::from_str(&data).map_err(|error| format!("配置文件格式错误：{error}"))?;
        settings.validate()?;
        return Ok((settings, path));
    }

    let mut settings = Settings::default();
    settings.validate()?;
    save_to(&path, &settings)?;
    Ok((settings, path))
}

pub fn save_to(path: &Path, settings: &Settings) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "配置路径缺少父目录".to_string())?;
    ensure_config_dir(parent).map_err(|error| format!("创建配置目录失败：{error}"))?;
    let payload =
        serde_json::to_vec_pretty(settings).map_err(|error| format!("序列化配置失败：{error}"))?;
    write_config_atomically(path, parent, &payload)
        .map_err(|error| format!("保存配置失败：{error}"))?;
    Ok(())
}

pub fn random_pairing_key() -> String {
    random_hex(32)
}

pub fn format_pairing_key(value: &str) -> String {
    value
        .as_bytes()
        .chunks(8)
        .map(|chunk| String::from_utf8_lossy(chunk).to_string())
        .collect::<Vec<_>>()
        .join("-")
}

fn normalize_pairing_key(value: &str) -> Result<String, String> {
    let normalized: String = value
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '-')
        .flat_map(char::to_lowercase)
        .collect();
    if normalized.len() != 64
        || !normalized
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err("配对密钥应为 64 位十六进制字符（可包含分组横线）".into());
    }
    Ok(normalized)
}

fn decode_hex_32(value: &str) -> Result<[u8; 32], String> {
    let normalized = normalize_pairing_key(value)?;
    let mut output = [0_u8; 32];
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&normalized[index * 2..index * 2 + 2], 16)
            .map_err(|_| "配对密钥包含无效字符".to_string())?;
    }
    Ok(output)
}

fn random_hex(byte_count: usize) -> String {
    let mut bytes = vec![0_u8; byte_count];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("operating system random generator unavailable");
    let mut output = String::with_capacity(byte_count * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn default_device_name() -> String {
    let raw = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .unwrap_or_default();
    truncate_device_name(&raw)
}

fn truncate_device_name(raw: &str) -> String {
    let truncated: String = raw.trim().chars().take(48).collect();
    let trimmed = truncated.trim();
    if trimmed.is_empty() {
        "我的电脑".to_string()
    } else {
        trimmed.to_string()
    }
}

fn validate_manual_address(value: &str) -> Result<(), String> {
    let host = if value.starts_with('[') {
        value
            .split_once(']')
            .map(|(host, _)| host.trim_start_matches('['))
            .unwrap_or(value)
    } else {
        value.split(':').next().unwrap_or(value)
    };
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return Err("手动地址格式无效".into());
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_config_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn ensure_config_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

#[cfg(unix)]
fn write_config_atomically(path: &Path, parent: &Path, payload: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings.json");
    let temp_path = parent.join(format!(".{file_name}.{}.tmp", random_hex(8)));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp_path)?;
    if let Err(error) = file.write_all(payload).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    drop(file);
    if let Err(error) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
}

#[cfg(not(unix))]
fn write_config_atomically(path: &Path, _parent: &Path, payload: &[u8]) -> io::Result<()> {
    fs::write(path, payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_key_accepts_grouped_input() {
        let raw = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let grouped = format_pairing_key(raw);
        assert_eq!(normalize_pairing_key(&grouped).unwrap(), raw);
        assert_eq!(decode_hex_32(&grouped).unwrap()[0], 0x00);
        assert_eq!(decode_hex_32(&grouped).unwrap()[31], 0xff);
    }

    #[test]
    fn invalid_pairing_key_is_rejected() {
        assert!(normalize_pairing_key("123456").is_err());
    }

    #[test]
    fn long_device_name_is_truncated_to_48_chars() {
        let long_name = "a".repeat(80);
        let truncated = truncate_device_name(&long_name);
        assert_eq!(truncated.chars().count(), 48);
        assert_eq!(truncate_device_name("   "), "我的电脑");
    }

    #[cfg(unix)]
    #[test]
    fn save_to_writes_atomically_with_restricted_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("deskbridge-cfg-test-{}", random_hex(8)));
        let path = dir.join("settings.json");
        let settings = Settings::default();
        save_to(&path, &settings).unwrap();

        let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        let file_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(file_mode, 0o600);

        let _ = fs::remove_dir_all(&dir);
    }
}
