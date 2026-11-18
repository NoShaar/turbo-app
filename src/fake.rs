//! Backend em memória para testes e para o modo de demonstração no Linux.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use crate::backend::SystemBackend;
use crate::model::*;

#[derive(Debug, Clone)]
struct RegKey {
    name: String,
    value: RegValue,
}

#[derive(Debug, Clone)]
pub struct FakeBackend {
    registry: Vec<(Hive, String, RegKey)>,
    pub services: BTreeMap<String, ServiceInfo>,
    pub power: String,
    mouse: MouseAccel,
    pub mouse_speed: u32,
    sticky: u32,
    filter: u32,
    toggle: u32,
    pub display: DisplayInfo,
    disks: Vec<DiskHealth>,
    events: Vec<DiskEventGroup>,
    pub disk_report: DiskReport,
    location: WindowsLocation,
    adapters: Vec<AdapterInfo>,
    pub printers: Vec<String>,
    startup: Vec<StartupEntry>,
    security: SecurityStatus,
    pub protection_enabled: bool,
    pub restore_frequency: Duration,
    pub fail_adapters: Option<String>,
    restore_points: Vec<(RestorePointInfo, SystemTime)>,
    pub elevated: bool,
    pub fail_writes: bool,
    pub deny_admin_reads: bool,
    pub resource_monitor_opens: u32,
    pub next_sequence: i64,
    /// Relógio injetável. `None` usa o relógio real.
    pub now: Option<SystemTime>,
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self::healthy()
    }
}

impl FakeBackend {
    pub fn healthy() -> Self {
        Self {
            registry: Vec::new(),
            services: BTreeMap::new(),
            power: ULTIMATE_PERFORMANCE_GUID.to_string(),
            mouse: MouseAccel { threshold1: 0, threshold2: 0, enabled: 0 },
            mouse_speed: 10,
            sticky: 506,
            filter: 0,
            toggle: 0,
            display: DisplayInfo {
                monitors: vec![MonitorMode {
                    device: String::new(),
                    label: "Principal".into(),
                    width: 1920,
                    height: 1080,
                    hz: 240,
                    max_hz: 240,
                }],
            },
            disks: vec![],
            events: vec![],
            disk_report: DiskReport {
                disks: vec![],
                top_processes: vec![],
                sysmain_is_top_on_hdd: false,
            },
            location: WindowsLocation {
                system_drive: "C:".into(),
                system_disk: "NVMe".into(),
                system_media: MediaKind::Nvme,
                pagefile_path: r"C:\pagefile.sys".into(),
                pagefile_size_mb: 5120,
                pagefile_media: MediaKind::Nvme,
            },
            adapters: vec![],
            printers: vec![],
            startup: vec![],
            security: SecurityStatus {
                secure_boot: Some(true),
                tpm_present: Some(true),
                tpm_ready: Some(true),
                spec_version: "2.0, 0, 1.38".into(),
            },
            protection_enabled: true,
            restore_frequency: Duration::from_secs(24 * 3600),
            fail_adapters: None,
            restore_points: vec![],
            elevated: true,
            fail_writes: false,
            deny_admin_reads: false,
            resource_monitor_opens: 0,
            next_sequence: 1,
            now: None,
        }
    }

    /// PC de exemplo alinhado à leitura descrita na especificação.
    pub fn carlos_demo() -> Self {
        let mut s = Self::healthy();
        s.elevated = false;
        s.deny_admin_reads = true;
        s.power = ULTIMATE_PERFORMANCE_GUID.into();
        s.mouse = MouseAccel { threshold1: 6, threshold2: 10, enabled: 1 };
        s.mouse_speed = 10;
        s.sticky = 510;
        s.filter = 510;
        s.toggle = 510;
        s.put(Hive::Hkcu, r"System\GameConfigStore", "GameDVR_Enabled", RegValue::Dword(1));
        s.put(
            Hive::Hkcu,
            r"Software\Microsoft\Windows\CurrentVersion\GameDVR",
            "AppCaptureEnabled",
            RegValue::Dword(1),
        );
        s.put(
            Hive::Hkcu,
            r"Software\Microsoft\Windows\CurrentVersion\GameDVR",
            "HistoricalCaptureEnabled",
            RegValue::Dword(1),
        );
        s.services.insert(
            "SysMain".into(),
            ServiceInfo { name: "SysMain".into(), run: ServiceRun::Running, start: StartType::Auto },
        );
        s.services.insert(
            "Spooler".into(),
            ServiceInfo { name: "Spooler".into(), run: ServiceRun::Running, start: StartType::Auto },
        );
        s.disks = vec![
            DiskHealth {
                name: "NVMe Kingston NV3".into(),
                media: MediaKind::Nvme,
                bus: "NVMe".into(),
                health: "Healthy".into(),
                temp_c: Some(41),
                temp_max_c: Some(85),
                wear: Some(2),
                power_on_hours: Some(1200),
                read_errors: Some(0),
                write_errors: Some(0),
            },
            DiskHealth {
                name: "SSD XrayDisk".into(),
                media: MediaKind::Ssd,
                bus: "SATA".into(),
                health: "Healthy".into(),
                temp_c: Some(37),
                temp_max_c: Some(70),
                wear: Some(8),
                power_on_hours: Some(4000),
                read_errors: Some(0),
                write_errors: Some(0),
            },
            DiskHealth {
                name: "HD WD 500GB".into(),
                media: MediaKind::Hdd,
                bus: "SATA".into(),
                health: "Healthy".into(),
                temp_c: Some(34),
                temp_max_c: Some(60),
                wear: None,
                power_on_hours: Some(22000),
                read_errors: Some(0),
                write_errors: Some(0),
            },
        ];
        s.disk_report = DiskReport {
            disks: vec![
                DiskActivity { name: "NVMe Kingston NV3".into(), media: MediaKind::Nvme, busy_percent: 4.0, queue: 0.02, sec_per_transfer: 0.0004 },
                DiskActivity { name: "HD WD 500GB".into(), media: MediaKind::Hdd, busy_percent: 12.0, queue: 0.1, sec_per_transfer: 0.008 },
            ],
            top_processes: vec![
                ProcessIo { name: "MsMpEng".into(), bytes_per_sec: 1_200_000.0 },
                ProcessIo { name: "explorer".into(), bytes_per_sec: 400_000.0 },
            ],
            sysmain_is_top_on_hdd: false,
        };
        s.adapters = vec![AdapterInfo {
            name: "Ethernet".into(),
            wired: true,
            connected: true,
            link_bps: 1_000_000_000,
            max_bps: 1_000_000_000,
            power_props: vec![
                NicPowerProp {
                    display_name: "Energia Eficiente Ethernet".into(),
                    keyword: "*EEE".into(),
                    current: "1".into(),
                    current_display: "Ativado".into(),
                    disabled_value: "0".into(),
                    disabled_display: "Desativado".into(),
                },
                NicPowerProp {
                    display_name: "Power Saving Mode".into(),
                    keyword: "PowerSavingMode".into(),
                    current: "1".into(),
                    current_display: "Ativado".into(),
                    disabled_value: "0".into(),
                    disabled_display: "Desativado".into(),
                },
            ],
        }];
        s.startup = vec![
            StartupEntry {
                name: "Opera GX Browser Assistant".into(),
                command: r"C:\Program Files\Opera GX\assistant\browser_assistant.exe".into(),
                location: StartupLocation::HkcuRun,
                approved: None,
            },
            StartupEntry {
                name: "MicrosoftEdgeAutoLaunch".into(),
                command: r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe --no-startup-window".into(),
                location: StartupLocation::HkcuRun,
                approved: Some(vec![0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            },
            StartupEntry {
                name: "SecurityHealth".into(),
                command: r"C:\Windows\System32\SecurityHealthSystray.exe".into(),
                location: StartupLocation::HklmRun,
                approved: None,
            },
            StartupEntry {
                name: "RTHDVCPL".into(),
                command: r"C:\Program Files\Realtek\Audio\HDA\RtkNGUI64.exe".into(),
                location: StartupLocation::HklmRun,
                approved: None,
            },
        ];
        s.put(Hive::Hkcu, r"Software\Microsoft\Windows\CurrentVersion\AdvertisingInfo", "Enabled", RegValue::Dword(1));
        s
    }

    pub fn put(&mut self, hive: Hive, path: &str, name: &str, value: RegValue) {
        if let Some(slot) = self.find_mut(hive, path, name) {
            slot.2.value = value;
            return;
        }
        self.registry.push((
            hive,
            path.to_string(),
            RegKey { name: name.to_string(), value },
        ));
    }

    fn find_mut(&mut self, hive: Hive, path: &str, name: &str) -> Option<&mut (Hive, String, RegKey)> {
        self.registry.iter_mut().find(|e| e.0 == hive && eq_ci(&e.1, path) && eq_ci(&e.2.name, name))
    }

    fn find(&self, hive: Hive, path: &str, name: &str) -> Option<&RegKey> {
        self.registry.iter().find(|e| e.0 == hive && eq_ci(&e.1, path) && eq_ci(&e.2.name, name)).map(|e| &e.2)
    }

    fn clock(&self) -> SystemTime {
        self.now.unwrap_or_else(SystemTime::now)
    }

    pub fn add_restore_point_at(&mut self, description: &str, when: SystemTime) {
        let info = RestorePointInfo {
            sequence: self.next_sequence,
            description: description.to_string(),
            created_rfc3339: "injetado".into(),
        };
        self.next_sequence += 1;
        self.restore_points.push((info, when));
    }
}

fn eq_ci(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

impl SystemBackend for FakeBackend {
    fn reg_get(&self, hive: Hive, path: &str, name: &str) -> Result<Option<RegValue>, BackendError> {
        Ok(self.find(hive, path, name).map(|k| k.value.clone()))
    }

    fn reg_set(&mut self, hive: Hive, path: &str, name: &str, value: &RegValue) -> Result<(), BackendError> {
        if self.fail_writes {
            return Ok(());
        }
        self.put(hive, path, name, value.clone());
        self.sync_startup(hive, path, name, Some(value));
        Ok(())
    }

    fn reg_delete(&mut self, hive: Hive, path: &str, name: &str) -> Result<(), BackendError> {
        if self.fail_writes {
            return Ok(());
        }
        self.registry.retain(|e| !(e.0 == hive && eq_ci(&e.1, path) && eq_ci(&e.2.name, name)));
        self.sync_startup(hive, path, name, None);
        Ok(())
    }

    fn reg_list(&self, hive: Hive, path: &str) -> Result<Vec<(String, RegValue)>, BackendError> {
        Ok(self
            .registry
            .iter()
            .filter(|e| e.0 == hive && eq_ci(&e.1, path))
            .map(|e| (e.2.name.clone(), e.2.value.clone()))
            .collect())
    }

    fn service(&self, name: &str) -> Result<ServiceInfo, BackendError> {
        self.services
            .get(name)
            .cloned()
            .ok_or_else(|| BackendError::Failed(format!("serviço {name} não existe")))
    }

    fn set_service_start(&mut self, name: &str, start: StartType) -> Result<(), BackendError> {
        if self.fail_writes {
            return Ok(());
        }
        let svc = self.services.get_mut(name).ok_or_else(|| BackendError::Failed(format!("serviço {name} não existe")))?;
        svc.start = start;
        Ok(())
    }

    fn start_service(&mut self, name: &str) -> Result<(), BackendError> {
        if self.fail_writes {
            return Ok(());
        }
        let svc = self.services.get_mut(name).ok_or_else(|| BackendError::Failed(format!("serviço {name} não existe")))?;
        if svc.start == StartType::Disabled {
            return Err(BackendError::Failed(format!("{name} está desativado")));
        }
        svc.run = ServiceRun::Running;
        Ok(())
    }

    fn stop_service(&mut self, name: &str) -> Result<(), BackendError> {
        if self.fail_writes {
            return Ok(());
        }
        let svc = self.services.get_mut(name).ok_or_else(|| BackendError::Failed(format!("serviço {name} não existe")))?;
        svc.run = ServiceRun::Stopped;
        Ok(())
    }

    fn power_active(&self) -> Result<String, BackendError> {
        Ok(self.power.clone())
    }

    fn power_set_active(&mut self, guid: &str) -> Result<(), BackendError> {
        if !self.fail_writes {
            self.power = normalize_guid(guid);
        }
        Ok(())
    }

    fn mouse_accel(&self) -> Result<MouseAccel, BackendError> {
        Ok(self.mouse)
    }

    fn set_mouse_accel(&mut self, value: MouseAccel) -> Result<(), BackendError> {
        if !self.fail_writes {
            self.mouse = value;
        }
        Ok(())
    }

    fn mouse_speed(&self) -> Result<u32, BackendError> {
        Ok(self.mouse_speed)
    }

    fn set_mouse_speed(&mut self, speed: u32) -> Result<(), BackendError> {
        if !self.fail_writes {
            self.mouse_speed = speed;
        }
        Ok(())
    }

    fn sticky_flags(&self) -> Result<u32, BackendError> {
        Ok(self.sticky)
    }
    fn set_sticky_flags(&mut self, flags: u32) -> Result<(), BackendError> {
        if !self.fail_writes {
            self.sticky = flags;
        }
        Ok(())
    }
    fn filter_flags(&self) -> Result<u32, BackendError> {
        Ok(self.filter)
    }
    fn set_filter_flags(&mut self, flags: u32) -> Result<(), BackendError> {
        if !self.fail_writes {
            self.filter = flags;
        }
        Ok(())
    }
    fn toggle_flags(&self) -> Result<u32, BackendError> {
        Ok(self.toggle)
    }
    fn set_toggle_flags(&mut self, flags: u32) -> Result<(), BackendError> {
        if !self.fail_writes {
            self.toggle = flags;
        }
        Ok(())
    }

    fn display(&self) -> Result<DisplayInfo, BackendError> {
        Ok(self.display.clone())
    }

    fn set_refresh(&mut self, device: &str, hz: u32) -> Result<(), BackendError> {
        if self.fail_writes {
            return Ok(());
        }
        let mon = self.display.monitors.iter_mut().find(|m| m.device == device).ok_or_else(|| BackendError::Failed("monitor não encontrado".into()))?;
        if hz > mon.max_hz {
            return Err(BackendError::Failed("Hz acima do máximo do modo atual".into()));
        }
        mon.hz = hz;
        Ok(())
    }

    fn disks(&self) -> Result<Vec<DiskHealth>, BackendError> {
        if self.deny_admin_reads && !self.elevated {
            return Err(BackendError::AccessDenied("contadores SMART exigem administrador".into()));
        }
        Ok(self.disks.clone())
    }

    fn disk_events(&self) -> Result<Vec<DiskEventGroup>, BackendError> {
        Ok(self.events.clone())
    }

    fn sample_disk_usage(&mut self) -> Result<DiskReport, BackendError> {
        Ok(self.disk_report.clone())
    }

    fn windows_location(&self) -> Result<WindowsLocation, BackendError> {
        Ok(self.location.clone())
    }

    fn adapters(&self) -> Result<Vec<AdapterInfo>, BackendError> {
        if let Some(reason) = &self.fail_adapters {
            return Err(BackendError::Failed(reason.clone()));
        }
        Ok(self.adapters.clone())
    }

    fn set_adapter_keyword(&mut self, adapter: &str, keyword: &str, value: &str) -> Result<(), BackendError> {
        if self.fail_writes {
            return Ok(());
        }
        let ad = self.adapters.iter_mut().find(|a| a.name.eq_ignore_ascii_case(adapter)).ok_or_else(|| BackendError::Failed("adaptador não encontrado".into()))?;
        let prop = ad.power_props.iter_mut().find(|p| p.keyword.eq_ignore_ascii_case(keyword)).ok_or_else(|| BackendError::Failed("propriedade não encontrada".into()))?;
        prop.current = value.to_string();
        if value == prop.disabled_value {
            prop.current_display = prop.disabled_display.clone();
        }
        Ok(())
    }

    fn physical_printers(&self) -> Result<Vec<String>, BackendError> {
        Ok(self.printers.clone())
    }

    fn startup_entries(&self) -> Result<Vec<StartupEntry>, BackendError> {
        Ok(self.startup.clone())
    }

    fn security_status(&self) -> Result<SecurityStatus, BackendError> {
        if self.deny_admin_reads && !self.elevated {
            return Err(BackendError::AccessDenied("TPM e Secure Boot exigem administrador para ler".into()));
        }
        Ok(self.security.clone())
    }

    fn protection_enabled(&self) -> Result<bool, BackendError> {
        Ok(self.protection_enabled)
    }

    fn set_system_protection(&mut self, enabled: bool) -> Result<(), BackendError> {
        self.protection_enabled = enabled;
        Ok(())
    }

    fn recent_restore_point(&self) -> Result<Option<RestorePointInfo>, BackendError> {
        if self.restore_frequency.is_zero() {
            return Ok(None);
        }
        let now = self.clock();
        let mut best: Option<(RestorePointInfo, SystemTime)> = None;
        for (info, when) in &self.restore_points {
            if now.duration_since(*when).unwrap_or(Duration::ZERO) <= self.restore_frequency {
                if best.as_ref().map(|(_, t)| when > t).unwrap_or(true) {
                    best = Some((info.clone(), *when));
                }
            }
        }
        Ok(best.map(|(info, _)| info))
    }

    fn restore_point_visible(&self, sequence: i64) -> Result<bool, BackendError> {
        Ok(self.restore_points.iter().any(|(info, _)| info.sequence == sequence))
    }

    fn create_restore_point(&mut self, description: &str) -> Result<RestorePointInfo, BackendError> {
        if !self.protection_enabled {
            return Err(BackendError::Failed("PROTECTION_DISABLED".into()));
        }
        if self.recent_restore_point()?.is_some() {
            return Err(BackendError::Failed("RESTORE_POINT_24H".into()));
        }
        let info = RestorePointInfo {
            sequence: self.next_sequence,
            description: description.to_string(),
            created_rfc3339: now_rfc3339(),
        };
        self.next_sequence += 1;
        self.restore_points.push((info.clone(), self.clock()));
        Ok(info)
    }

    fn is_elevated(&self) -> bool {
        self.elevated
    }

    fn open_resource_monitor(&mut self) -> Result<(), BackendError> {
        self.resource_monitor_opens += 1;
        Ok(())
    }
}

impl FakeBackend {
    fn sync_startup(&mut self, hive: Hive, path: &str, name: &str, value: Option<&RegValue>) {
        let Some(location) = startup_location_for(hive, path) else {
            return;
        };
        let approved = match value {
            Some(RegValue::Binary(bytes)) => Some(bytes.clone()),
            None => None,
            Some(_) => return,
        };
        if let Some(entry) = self
            .startup
            .iter_mut()
            .find(|e| e.location == location && e.name.eq_ignore_ascii_case(name))
        {
            entry.approved = approved;
        }
    }
}

fn startup_location_for(hive: Hive, path: &str) -> Option<StartupLocation> {
    let p = path.to_ascii_lowercase();
    if p.ends_with(r"\startupapproved\run32") {
        return Some(StartupLocation::HklmRun32);
    }
    if p.ends_with(r"\startupapproved\startupfolder") {
        return Some(match hive {
            Hive::Hkcu => StartupLocation::UserStartupFolder,
            Hive::Hklm => StartupLocation::CommonStartupFolder,
        });
    }
    if p.ends_with(r"\startupapproved\run") {
        return Some(match hive {
            Hive::Hkcu => StartupLocation::HkcuRun,
            Hive::Hklm => StartupLocation::HklmRun,
        });
    }
    None
}
