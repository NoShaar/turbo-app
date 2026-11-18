//! Tipos compartilhados pelo motor, pelo backup e pela interface.

use serde::{Deserialize, Serialize};

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Plano de alto desempenho documentado pela Microsoft (`powercfg /setactive`).
pub const HIGH_PERFORMANCE_GUID: &str = "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c";
pub const ULTIMATE_PERFORMANCE_GUID: &str = "e9a42b02-d5df-448d-aa00-03f14749eb61";
pub const BALANCED_GUID: &str = "381b4222-f694-41f0-9685-ff5bb260df2e";
pub const POWER_SAVER_GUID: &str = "a1841308-3541-4fab-bc81-f71556f20b4a";

pub const HOTKEY_ACTIVE: u32 = 0x4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    Disco,
    Desempenho,
    Mouse,
    Teclado,
    Monitor,
    Rede,
    Servicos,
    Inicio,
    Privacidade,
    Jogos,
}

impl Section {
    pub fn id(self) -> &'static str {
        match self {
            Self::Disco => "disco",
            Self::Desempenho => "desempenho",
            Self::Mouse => "mouse",
            Self::Teclado => "teclado",
            Self::Monitor => "monitor",
            Self::Rede => "rede",
            Self::Servicos => "servicos",
            Self::Inicio => "inicio",
            Self::Privacidade => "privacidade",
            Self::Jogos => "jogos",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Disco => "Saúde do disco",
            Self::Desempenho => "FPS e desempenho",
            Self::Mouse => "Mouse",
            Self::Teclado => "Teclado",
            Self::Monitor => "Monitor e GPU",
            Self::Rede => "Rede",
            Self::Servicos => "Serviços",
            Self::Inicio => "Inicialização e background",
            Self::Privacidade => "Windows e privacidade",
            Self::Jogos => "Jogos",
        }
    }

    pub fn all() -> &'static [Section] {
        &[
            Self::Disco,
            Self::Desempenho,
            Self::Mouse,
            Self::Teclado,
            Self::Monitor,
            Self::Rede,
            Self::Servicos,
            Self::Inicio,
            Self::Privacidade,
            Self::Jogos,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Seal {
    Seguro,
    Ressalva,
}

impl Seal {
    pub fn label(self) -> &'static str {
        match self {
            Self::Seguro => "SEGURO",
            Self::Ressalva => "SEGURO COM RESSALVA",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemState {
    Ok,
    CanImprove,
    NotRead,
    NeedsAdmin,
}

impl ItemState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::CanImprove => "PODE MELHORAR",
            Self::NotRead => "NÃO LIDO",
            Self::NeedsAdmin => "PRECISA DE ADMIN",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hive {
    Hkcu,
    Hklm,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum RegValue {
    Dword(u32),
    Sz(String),
    Binary(Vec<u8>),
    MultiSz(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegSlot {
    pub hive: Hive,
    pub path: String,
    pub name: String,
    pub value: Option<RegValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartType {
    Auto,
    Manual,
    Disabled,
}

impl StartType {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automático",
            Self::Manual => "Manual",
            Self::Disabled => "Desativado",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceRun {
    Running,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub run: ServiceRun,
    pub start: StartType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseAccel {
    pub threshold1: i32,
    pub threshold2: i32,
    pub enabled: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Unknown,
    Hdd,
    Ssd,
    Nvme,
}

impl MediaKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "desconhecido",
            Self::Hdd => "HD",
            Self::Ssd => "SSD",
            Self::Nvme => "NVMe",
        }
    }

    pub fn is_solid(self) -> bool {
        matches!(self, Self::Ssd | Self::Nvme)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiskHealth {
    pub name: String,
    pub media: MediaKind,
    pub bus: String,
    pub health: String,
    pub temp_c: Option<i32>,
    pub temp_max_c: Option<i32>,
    pub wear: Option<i32>,
    pub power_on_hours: Option<i64>,
    pub read_errors: Option<i64>,
    pub write_errors: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskEventGroup {
    pub id: u32,
    pub provider: String,
    pub count: u32,
    pub sample: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiskActivity {
    pub name: String,
    pub media: MediaKind,
    pub busy_percent: f32,
    pub queue: f32,
    pub sec_per_transfer: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessIo {
    pub name: String,
    pub bytes_per_sec: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiskReport {
    pub disks: Vec<DiskActivity>,
    pub top_processes: Vec<ProcessIo>,
    /// Verdadeiro só quando o processo do SysMain lidera o I/O de um HD mecânico.
    pub sysmain_is_top_on_hdd: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowsLocation {
    pub system_drive: String,
    pub system_disk: String,
    pub system_media: MediaKind,
    pub pagefile_path: String,
    pub pagefile_size_mb: u64,
    pub pagefile_media: MediaKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicPowerProp {
    pub display_name: String,
    pub keyword: String,
    pub current: String,
    pub current_display: String,
    pub disabled_value: String,
    pub disabled_display: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterInfo {
    pub name: String,
    pub wired: bool,
    pub connected: bool,
    pub link_bps: u64,
    pub max_bps: u64,
    pub power_props: Vec<NicPowerProp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorMode {
    pub device: String,
    pub label: String,
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub max_hz: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub monitors: Vec<MonitorMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupLocation {
    HkcuRun,
    HklmRun,
    HklmRun32,
    UserStartupFolder,
    CommonStartupFolder,
}

impl StartupLocation {
    pub fn needs_admin(self) -> bool {
        matches!(
            self,
            Self::HklmRun | Self::HklmRun32 | Self::CommonStartupFolder
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::HkcuRun => "HKCU Run",
            Self::HklmRun => "HKLM Run",
            Self::HklmRun32 => "HKLM Run32",
            Self::UserStartupFolder => "Pasta Inicializar",
            Self::CommonStartupFolder => "Pasta Inicializar (todos)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupEntry {
    pub name: String,
    pub command: String,
    pub location: StartupLocation,
    /// `None` significa que não existe valor em StartupApproved (o Windows trata como ativo).
    pub approved: Option<Vec<u8>>,
}

impl StartupEntry {
    pub fn enabled(&self) -> bool {
        match self.approved.as_deref() {
            None => true,
            Some(bytes) if bytes.is_empty() => true,
            Some(bytes) => matches!(bytes[0], 0x02 | 0x06),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityStatus {
    pub secure_boot: Option<bool>,
    pub tpm_present: Option<bool>,
    pub tpm_ready: Option<bool>,
    pub spec_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestorePointInfo {
    pub sequence: i64,
    pub description: String,
    pub created_rfc3339: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestorePointStatus {
    Created,
    UsedExisting,
    ProtectionDisabled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestorePointRecord {
    pub status: RestorePointStatus,
    pub message: String,
    pub sequence: Option<i64>,
    pub acknowledged_no_protection: bool,
    /// O Turbo ligou a Proteção do Sistema para criar o ponto. Reverter tudo desliga de novo.
    #[serde(default)]
    pub protection_enabled_by_app: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemSnapshot {
    Registry {
        entries: Vec<RegSlot>,
    },
    Power {
        guid: String,
    },
    MouseAccel {
        threshold1: i32,
        threshold2: i32,
        enabled: i32,
    },
    MouseSpeed {
        speed: u32,
    },
    Hotkeys {
        sticky: u32,
        filter: u32,
        toggle: u32,
    },
    Refresh {
        monitors: Vec<MonitorMode>,
    },
    Service {
        name: String,
        running: bool,
        start: StartType,
    },
    Nic {
        props: Vec<NicSnap>,
    },
    Startup {
        entries: Vec<StartupEntry>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicSnap {
    pub adapter: String,
    pub keyword: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupFile {
    pub version: u32,
    pub app_version: String,
    pub created_at: String,
    pub restore_point: RestorePointRecord,
    pub items: Vec<BackupItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupItem {
    pub id: String,
    pub snapshot: ItemSnapshot,
}

impl BackupFile {
    pub fn snapshot(&self, id: &str) -> Option<&ItemSnapshot> {
        self.items.iter().find(|i| i.id == id).map(|i| &i.snapshot)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemView {
    pub id: String,
    pub section: Section,
    pub name: String,
    pub faz: String,
    pub ajuda: String,
    pub ok_when: String,
    pub admin: String,
    pub reinicio: String,
    pub seal: Seal,
    pub caveat: String,
    pub readonly: bool,
    pub conditional: bool,
    pub state: ItemState,
    pub detail: String,
    pub can_optimize: bool,
    pub can_revert: bool,
    pub needs_admin_read: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("acesso negado: {0}")]
    AccessDenied(String),
    #[error("{0}")]
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{0}")]
    Message(String),
    #[error("Crie o backup antes de alterar qualquer coisa.")]
    BackupRequired,
    #[error("{message}")]
    ProtectionDisabled { message: String },
    #[error("Este item é só leitura. O Turbo mostra o estado e não altera nada.")]
    ReadOnly,
    #[error("É preciso confirmar a elevação do Windows (UAC) para aplicar este item.")]
    NeedsAdmin,
    #[error("{0}")]
    Backend(String),
}

impl From<BackendError> for EngineError {
    fn from(value: BackendError) -> Self {
        match value {
            BackendError::AccessDenied(msg) => {
                EngineError::Message(format!("Acesso negado. {msg}"))
            }
            BackendError::Failed(msg) => EngineError::Backend(msg),
        }
    }
}

pub fn power_name(guid: &str) -> String {
    let g = guid.trim().trim_matches(|c| c == '{' || c == '}').to_ascii_lowercase();
    match g.as_str() {
        HIGH_PERFORMANCE_GUID => "Alto desempenho".to_string(),
        ULTIMATE_PERFORMANCE_GUID => "Desempenho Máximo".to_string(),
        BALANCED_GUID => "Equilibrado".to_string(),
        POWER_SAVER_GUID => "Economia de energia".to_string(),
        _ => format!("Plano personalizado ({g})"),
    }
}

pub fn is_high_or_ultimate(guid: &str) -> bool {
    let g = guid.trim().trim_matches(|c| c == '{' || c == '}').to_ascii_lowercase();
    g == HIGH_PERFORMANCE_GUID || g == ULTIMATE_PERFORMANCE_GUID
}

pub fn is_saver_or_balanced(guid: &str) -> bool {
    let g = guid.trim().trim_matches(|c| c == '{' || c == '}').to_ascii_lowercase();
    g == BALANCED_GUID || g == POWER_SAVER_GUID
}

pub fn normalize_guid(guid: &str) -> String {
    guid.trim()
        .trim_matches(|c| c == '{' || c == '}')
        .to_ascii_lowercase()
}

/// Nomes que o app nunca desativa na inicialização: Segurança do Windows,
/// Vanguard e drivers de áudio/vídeo.
pub fn startup_protected(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    const EXACT: &[&str] = &[
        "securityhealth",
        "securityhealthsystray",
        "windowsdefender",
        "vgtray",
        "vgc",
        "rthdvcpl",
    ];
    if EXACT.iter().any(|e| *e == n.as_str()) {
        return true;
    }
    const PARTS: &[&str] = &[
        "realtek",
        "nahimic",
        "rtkaud",
        "nvcontainer",
        "nvdisplay",
        "nvidia",
        "amdrsserv",
        "radeonsoftware",
        "amddvr",
        "igfxtray",
        "igfxem",
        "igfxhk",
        "waves",
    ];
    PARTS.iter().any(|p| n.contains(p))
}

pub fn startup_disabled_blob() -> Vec<u8> {
    let mut bytes = vec![0x03, 0x00, 0x00, 0x00];
    bytes.extend_from_slice(&filetime_now());
    bytes
}

fn filetime_now() -> [u8; 8] {
    // FILETIME: 100 ns desde 1601-01-01. O valor exato não muda o estado
    // (o primeiro byte é que marca desativado); gravamos a hora atual.
    let unix_ms: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;
    let ft = unix_ms.saturating_mul(10_000).saturating_add(EPOCH_DIFF_100NS);
    ft.to_le_bytes()
}

pub fn bps_label(bps: u64) -> String {
    if bps == 0 {
        return "sem link".to_string();
    }
    let mbps = bps as f64 / 1_000_000.0;
    if mbps >= 1000.0 {
        format!("{:.0} Gbps", mbps / 1000.0)
    } else {
        format!("{mbps:.0} Mbps")
    }
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
