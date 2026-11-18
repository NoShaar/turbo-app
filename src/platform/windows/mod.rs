//! Backend do Windows 11. Registro, serviços, WMI, SPI, energia, tela e
//! contadores. Sem PowerShell.

#![allow(non_snake_case)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use wmi::{COMLibrary, WMIConnection};

use crate::backend::SystemBackend;
use crate::model::*;

use windows::core::{w, GUID, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PRIVILEGE_NOT_HELD, HANDLE, HLOCAL, LUID, WIN32_ERROR,
};
use windows::Win32::Graphics::Gdi::{
    ChangeDisplaySettingsExW, EnumDisplayDevicesW, EnumDisplaySettingsW, CDS_UPDATEREGISTRY, DEVMODEW, DEVMODE_FIELD_FLAGS, DISPLAY_DEVICEW, DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE,
};
use windows::Win32::Graphics::Printing::{EnumPrintersW, PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL, PRINTER_INFO_2W};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::EventLog::{EvtClose, EvtNext, EvtQuery, EvtRender, EvtQueryReverseDirection, EvtRenderEventXml, EVT_HANDLE};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
};
use windows::Win32::System::Power::{
    GetSystemPowerStatus, PowerDeleteScheme, PowerDuplicateScheme, PowerEnumerate, PowerGetActiveScheme, PowerReadACValueIndex, PowerReadFriendlyName, PowerSetActiveScheme, ACCESS_SCHEME, SYSTEM_POWER_STATUS,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Wmi::{IWbemContext, WBEM_GENERIC_FLAG_TYPE};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, REG_BINARY, REG_DWORD, REG_EXPAND_SZ, REG_MULTI_SZ, REG_SZ, REG_VALUE_TYPE,
};
use windows::Win32::System::Restore::{SRSetRestorePointW, BEGIN_SYSTEM_CHANGE, END_SYSTEM_CHANGE, MODIFY_SETTINGS, RESTOREPOINTINFOW, STATEMGRSTATUS};
use windows::Win32::System::Services::{
    ChangeServiceConfigW, CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceConfigW, QueryServiceStatusEx, StartServiceW, ENUM_SERVICE_TYPE, QUERY_SERVICE_CONFIGW, SC_HANDLE, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_AUTO_START, SERVICE_CONTROL_STOP, SERVICE_DEMAND_START, SERVICE_DISABLED, SERVICE_ERROR, SERVICE_NO_CHANGE, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START, SERVICE_STATUS, SERVICE_STATUS_PROCESS,
};
use windows::Win32::System::SystemInformation::GetWindowsDirectoryW;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::System::WindowsProgramming::GetFirmwareEnvironmentVariableW;
use windows::Win32::UI::Accessibility::{FILTERKEYS, STICKYKEYS, TOGGLEKEYS};
use windows::Win32::UI::Shell::{IsUserAnAdmin, SHGetKnownFolderPath, FOLDERID_CommonStartup, FOLDERID_Startup, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETFILTERKEYS, SPI_GETMOUSE, SPI_GETMOUSESPEED, SPI_GETSTICKYKEYS, SPI_GETTOGGLEKEYS, SPI_SETFILTERKEYS, SPI_SETMOUSE, SPI_SETMOUSESPEED, SPI_SETSTICKYKEYS, SPI_SETTOGGLEKEYS, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

const NET_CLASS: &str = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e972-e325-11ce-bfc1-08002be10318}";

static UI_THREAD: std::sync::OnceLock<std::thread::ThreadId> = std::sync::OnceLock::new();

/// Marca a thread da janela. `WindowsBackend::new` recusa essa thread porque
/// o `wmi` chama `CoInitializeEx(COINIT_MULTITHREADED)` e o winit precisa de STA.
pub fn mark_ui_thread() {
    let _ = UI_THREAD.set(std::thread::current().id());
}

pub struct WindowsBackend {
    _com: COMLibrary,
    created_power: Option<String>,
}

/// Solta o `COMLibrary` e só então chama `CoUninitialize`.
struct ComSession {
    backend: Option<WindowsBackend>,
}

impl Drop for ComSession {
    fn drop(&mut self) {
        self.backend.take();
        unsafe { windows::Win32::System::Com::CoUninitialize() };
    }
}

impl WindowsBackend {
    /// Inicializa COM em MTA. Só pode ser chamado numa thread de trabalho.
    pub fn new() -> Result<Self, String> {
        if UI_THREAD.get().copied() == Some(std::thread::current().id()) {
            return Err("COM multithread não pode ser inicializado na thread da janela".into());
        }
        let com = COMLibrary::new().map_err(|e| e.to_string())?;
        Ok(Self { _com: com, created_power: None })
    }

    /// Cria o backend, executa `f` e desfaz o COM nesta thread.
    pub fn with_session<T>(f: impl FnOnce(&mut Self) -> T) -> Result<T, String> {
        let mut session = ComSession { backend: Some(Self::new()?) };
        Ok(f(session.backend.as_mut().expect("sessão COM")))
    }

    fn wmi(&self, namespace: &str) -> Result<WMIConnection, BackendError> {
        let com = unsafe { COMLibrary::assume_initialized() };
        let _ = &self._com;
        WMIConnection::with_namespace_path(namespace, com).map_err(wmi_err)
    }
}

impl SystemBackend for WindowsBackend {
    fn reg_get(&self, hive: Hive, path: &str, name: &str) -> Result<Option<RegValue>, BackendError> {
        let Some(key) = open_key(hive, path, KEY_READ.0)? else {
            return Ok(None);
        };
        let result = query_value(key.0, name);
        result
    }

    fn reg_set(&mut self, hive: Hive, path: &str, name: &str, value: &RegValue) -> Result<(), BackendError> {
        let key = create_key(hive, path)?;
        set_value(key.0, name, value)
    }

    fn reg_delete(&mut self, hive: Hive, path: &str, name: &str) -> Result<(), BackendError> {
        let Some(key) = open_key(hive, path, KEY_SET_VALUE.0)? else {
            return Ok(());
        };
        let name_w = wide(name);
        let err = unsafe { RegDeleteValueW(key.0, PCWSTR(name_w.as_ptr())) };
        if err == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        win_ok(err, "apagar valor do registro")
    }

    fn reg_list(&self, hive: Hive, path: &str) -> Result<Vec<(String, RegValue)>, BackendError> {
        let Some(key) = open_key(hive, path, KEY_READ.0)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut index = 0u32;
        loop {
            let mut name_buf = vec![0u16; 256];
            let mut name_len = name_buf.len() as u32;
            let mut typ_raw = 0u32;
            let mut data_len = 0u32;
            let err = unsafe {
                RegEnumValueW(key.0, index, PWSTR(name_buf.as_mut_ptr()), &mut name_len, None, Some(&mut typ_raw), None, Some(&mut data_len))
            };
            if err == ERROR_FILE_NOT_FOUND || err.0 == 259 {
                break;
            }
            win_ok(err, "enumerar registro")?;
            let name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
            if let Some(value) = query_value(key.0, &name)? {
                out.push((name, value));
            }
            index += 1;
            if index > 10_000 {
                break;
            }
        }
        Ok(out)
    }

    fn service(&self, name: &str) -> Result<ServiceInfo, BackendError> {
        let handle = open_service(name, SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG)?;
        let mut needed = 0u32;
        let _ = unsafe { QueryServiceConfigW(handle.0, None, 0, &mut needed) };
        let mut buf = vec![0u8; needed.max(64) as usize];
        unsafe { QueryServiceConfigW(handle.0, Some(buf.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW), buf.len() as u32, &mut needed).map_err(|e| BackendError::Failed(e.to_string()))? };
        let cfg = unsafe { &*(buf.as_ptr() as *const QUERY_SERVICE_CONFIGW) };
        let start = match cfg.dwStartType {
            x if x == SERVICE_DISABLED => StartType::Disabled,
            x if x == SERVICE_DEMAND_START => StartType::Manual,
            _ => StartType::Auto,
        };
        let mut status = SERVICE_STATUS_PROCESS::default();
        let mut needed = std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32;
        unsafe {
            QueryServiceStatusEx(handle.0, SC_STATUS_PROCESS_INFO, Some(std::slice::from_raw_parts_mut(&mut status as *mut _ as *mut u8, needed as usize)), &mut needed)
                .map_err(|e| BackendError::Failed(e.to_string()))?;
        }
        let run = if status.dwCurrentState == SERVICE_RUNNING { ServiceRun::Running } else { ServiceRun::Stopped };
        Ok(ServiceInfo { name: name.to_string(), run, start })
    }

    fn set_service_start(&mut self, name: &str, start: StartType) -> Result<(), BackendError> {
        let handle = open_service(name, SERVICE_CHANGE_CONFIG())?;
        let kind = match start {
            StartType::Auto => SERVICE_AUTO_START,
            StartType::Manual => SERVICE_DEMAND_START,
            StartType::Disabled => SERVICE_DISABLED,
        };
        unsafe {
            ChangeServiceConfigW(handle.0, ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE), kind, SERVICE_ERROR(SERVICE_NO_CHANGE), PCWSTR::null(), PCWSTR::null(), None, PCWSTR::null(), PCWSTR::null(), PCWSTR::null(), PCWSTR::null())
                .map_err(|e| BackendError::Failed(e.to_string()))
        }
    }

    fn start_service(&mut self, name: &str) -> Result<(), BackendError> {
        let handle = open_service(name, SERVICE_START)?;
        unsafe { StartServiceW(handle.0, None).map_err(|e| BackendError::Failed(format!("iniciar {name}: {e}"))) }
    }

    fn stop_service(&mut self, name: &str) -> Result<(), BackendError> {
        let handle = open_service(name, SERVICE_STOP_RIGHT())?;
        let mut status = SERVICE_STATUS::default();
        unsafe {
            ControlService(handle.0, SERVICE_CONTROL_STOP, &mut status).map_err(|e| BackendError::Failed(format!("parar {name}: {e}")))?;
        }
        for _ in 0..20 {
            let info = self.service(name)?;
            if info.run == ServiceRun::Stopped {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(150));
        }
        Ok(())
    }

    fn power_active(&self) -> Result<String, BackendError> {
        unsafe {
            let mut ptr = std::ptr::null_mut();
            let err = PowerGetActiveScheme(HKEY::default(), &mut ptr);
            win_ok(err, "ler plano de energia")?;
            if ptr.is_null() {
                return Err(BackendError::Failed("plano de energia vazio".into()));
            }
            let guid = *ptr;
            let _ = LocalFree(HLOCAL(ptr as *mut _));
            Ok(guid_to_string(&guid))
        }
    }

    fn power_set_active(&mut self, guid: &str) -> Result<(), BackendError> {
        let guid = parse_guid(guid)?;
        unsafe {
            let err = PowerSetActiveScheme(HKEY::default(), Some(&guid));
            win_ok(err, "trocar plano de energia")
        }
    }

    fn power_schemes(&self) -> Result<Vec<PowerSchemeInfo>, BackendError> {
        read_power_schemes()
    }

    fn power_duplicate(&mut self, source_guid: &str) -> Result<String, BackendError> {
        let source = parse_guid(source_guid)?;
        unsafe {
            let mut dest = std::ptr::null_mut();
            let err = PowerDuplicateScheme(HKEY::default(), &source, &mut dest);
            win_ok(err, "duplicar plano de energia")?;
            if dest.is_null() {
                return Err(BackendError::Failed("o Windows não devolveu o GUID do plano criado".into()));
            }
            let guid = guid_to_string(&*dest);
            let _ = LocalFree(HLOCAL(dest as *mut _));
            crate::store::trace(&format!("energia duplicatescheme {source_guid} -> {guid}"));
            Ok(guid)
        }
    }

    fn power_delete(&mut self, guid: &str) -> Result<(), BackendError> {
        if crate::rules::is_builtin_scheme(guid) {
            return Err(BackendError::Failed("não apago um plano embutido do Windows".into()));
        }
        let parsed = parse_guid(guid)?;
        unsafe {
            let err = PowerDeleteScheme(HKEY::default(), &parsed);
            if err == ERROR_FILE_NOT_FOUND || err.0 == 2 {
                crate::store::trace(&format!("energia apagar {guid}: já não existia"));
                return Ok(());
            }
            win_ok(err, "apagar plano de energia")?;
        }
        crate::store::trace(&format!("energia apaguei {guid}"));
        Ok(())
    }

    fn note_created_power_scheme(&mut self, guid: &str) {
        self.created_power = Some(normalize_guid(guid));
    }

    fn take_created_power_scheme(&mut self) -> Option<String> {
        self.created_power.take()
    }

    fn machine_kind(&self) -> MachineKind {
        match power_status() {
            Some(status) if status.BatteryFlag == 128 || status.BatteryFlag == 255 => MachineKind::Desktop,
            Some(_) => MachineKind::Laptop,
            None => MachineKind::Desktop,
        }
    }

    fn on_battery(&self) -> bool {
        power_status().is_some_and(|status| status.ACLineStatus == 0)
    }

    fn mouse_accel(&self) -> Result<MouseAccel, BackendError> {
        let mut arr = [0i32; 3];
        spi_get(SPI_GETMOUSE, 0, arr.as_mut_ptr() as *mut _)?;
        Ok(MouseAccel { threshold1: arr[0], threshold2: arr[1], enabled: arr[2] })
    }

    fn set_mouse_accel(&mut self, value: MouseAccel) -> Result<(), BackendError> {
        let mut arr = [value.threshold1, value.threshold2, value.enabled];
        spi_set(SPI_SETMOUSE, 0, arr.as_mut_ptr() as *mut _)
    }

    fn mouse_speed(&self) -> Result<u32, BackendError> {
        let mut speed = 0i32;
        spi_get(SPI_GETMOUSESPEED, 0, &mut speed as *mut _ as *mut _)?;
        Ok(speed as u32)
    }

    fn set_mouse_speed(&mut self, speed: u32) -> Result<(), BackendError> {
        spi_set(SPI_SETMOUSESPEED, 0, speed as *mut _)
    }

    fn sticky_flags(&self) -> Result<u32, BackendError> {
        let mut info = STICKYKEYS { cbSize: std::mem::size_of::<STICKYKEYS>() as u32, dwFlags: Default::default() };
        spi_get(SPI_GETSTICKYKEYS, info.cbSize, &mut info as *mut _ as *mut _)?;
        Ok(info.dwFlags.0)
    }

    fn set_sticky_flags(&mut self, flags: u32) -> Result<(), BackendError> {
        let mut info = STICKYKEYS { cbSize: std::mem::size_of::<STICKYKEYS>() as u32, dwFlags: Default::default() };
        spi_get(SPI_GETSTICKYKEYS, info.cbSize, &mut info as *mut _ as *mut _)?;
        info.dwFlags.0 = flags;
        spi_set(SPI_SETSTICKYKEYS, info.cbSize, &mut info as *mut _ as *mut _)
    }

    fn filter_flags(&self) -> Result<u32, BackendError> {
        let mut info = FILTERKEYS { cbSize: std::mem::size_of::<FILTERKEYS>() as u32, ..Default::default() };
        spi_get(SPI_GETFILTERKEYS, info.cbSize, &mut info as *mut _ as *mut _)?;
        Ok(info.dwFlags)
    }

    fn set_filter_flags(&mut self, flags: u32) -> Result<(), BackendError> {
        let mut info = FILTERKEYS { cbSize: std::mem::size_of::<FILTERKEYS>() as u32, ..Default::default() };
        spi_get(SPI_GETFILTERKEYS, info.cbSize, &mut info as *mut _ as *mut _)?;
        info.dwFlags = flags;
        spi_set(SPI_SETFILTERKEYS, info.cbSize, &mut info as *mut _ as *mut _)
    }

    fn toggle_flags(&self) -> Result<u32, BackendError> {
        let mut info = TOGGLEKEYS { cbSize: std::mem::size_of::<TOGGLEKEYS>() as u32, dwFlags: 0 };
        spi_get(SPI_GETTOGGLEKEYS, info.cbSize, &mut info as *mut _ as *mut _)?;
        Ok(info.dwFlags)
    }

    fn set_toggle_flags(&mut self, flags: u32) -> Result<(), BackendError> {
        let mut info = TOGGLEKEYS { cbSize: std::mem::size_of::<TOGGLEKEYS>() as u32, dwFlags: 0 };
        spi_get(SPI_GETTOGGLEKEYS, info.cbSize, &mut info as *mut _ as *mut _)?;
        info.dwFlags = flags;
        spi_set(SPI_SETTOGGLEKEYS, info.cbSize, &mut info as *mut _ as *mut _)
    }

    fn display(&self) -> Result<DisplayInfo, BackendError> {
        Ok(DisplayInfo { monitors: read_monitors()? })
    }

    fn set_refresh(&mut self, device: &str, hz: u32) -> Result<(), BackendError> {
        let name = HSTRING::from(device);
        let device_pcw = if device.is_empty() { PCWSTR::null() } else { PCWSTR(name.as_ptr()) };
        let mut mode = read_devmode(device_pcw, ENUM_CURRENT_SETTINGS).ok_or_else(|| BackendError::Failed("não li o modo atual do monitor".into()))?;
        mode.dmDisplayFrequency = hz;
        mode.dmFields = DEVMODE_FIELD_FLAGS(mode.dmFields.0 | DM_DISPLAYFREQUENCY.0);
        let change = unsafe { ChangeDisplaySettingsExW(device_pcw, Some(&mode), windows::Win32::Foundation::HWND::default(), CDS_UPDATEREGISTRY, None) };
        if change == DISP_CHANGE_SUCCESSFUL {
            Ok(())
        } else {
            Err(BackendError::Failed(format!("ChangeDisplaySettingsEx devolveu {}", change.0)))
        }
    }

    fn disks(&self) -> Result<Vec<DiskHealth>, BackendError> {
        read_disks(self)
    }

    fn disk_events(&self) -> Result<Vec<DiskEventGroup>, BackendError> {
        read_disk_events()
    }

    fn sample_disk_usage(&mut self) -> Result<DiskReport, BackendError> {
        sample_disk(self)
    }

    fn windows_location(&self) -> Result<WindowsLocation, BackendError> {
        read_location(self)
    }

    fn adapters(&self) -> Result<Vec<AdapterInfo>, BackendError> {
        read_adapters(self)
    }

    fn set_adapter_keyword(&mut self, adapter: &str, interface_guid: &str, keyword: &str, value: &str) -> Result<(), BackendError> {
        set_nic_keyword(self, adapter, interface_guid, keyword, value)
    }

    fn restart_adapter(&mut self, adapter: &str, interface_guid: &str) -> Result<(), BackendError> {
        bounce_adapter(self, adapter, interface_guid)
    }

    fn physical_printers(&self) -> Result<Vec<String>, BackendError> {
        read_printers()
    }

    fn startup_entries(&self) -> Result<Vec<StartupEntry>, BackendError> {
        read_startup(self)
    }

    fn security_status(&self) -> Result<SecurityStatus, BackendError> {
        read_security(self)
    }

    fn protection_enabled(&self) -> Result<bool, BackendError> {
        match self.reg_get(Hive::Hklm, r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\SystemRestore", "DisableSR")? {
            Some(RegValue::Dword(1)) => Ok(false),
            _ => Ok(true),
        }
    }

    fn set_system_protection(&mut self, enabled: bool) -> Result<(), BackendError> {
        set_system_protection(self, enabled)
    }

    fn recent_restore_point(&self) -> Result<Option<RestorePointInfo>, BackendError> {
        recent_restore(self)
    }

    fn restore_point_visible(&self, sequence: i64) -> Result<bool, BackendError> {
        Ok(list_restore_points(self)?.iter().any(|info| info.sequence == sequence))
    }

    fn create_restore_point(&mut self, description: &str) -> Result<RestorePointInfo, BackendError> {
        create_restore(description)
    }

    fn is_elevated(&self) -> bool {
        unsafe { IsUserAnAdmin().as_bool() }
    }

    fn open_resource_monitor(&mut self) -> Result<(), BackendError> {
        std::process::Command::new("resmon.exe").spawn().map(|_| ()).map_err(|e| BackendError::Failed(format!("abrir o Monitor de Recursos: {e}")))
    }
}

struct RegHandle(HKEY);
impl Drop for RegHandle {
    fn drop(&mut self) {
        unsafe { let _ = RegCloseKey(self.0); }
    }
}

struct SvcHandle(SC_HANDLE);
impl Drop for SvcHandle {
    fn drop(&mut self) {
        unsafe { let _ = CloseServiceHandle(self.0); }
    }
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn hive_key(hive: Hive) -> HKEY {
    match hive {
        Hive::Hkcu => HKEY_CURRENT_USER,
        Hive::Hklm => HKEY_LOCAL_MACHINE,
    }
}

fn win_ok(err: WIN32_ERROR, what: &str) -> Result<(), BackendError> {
    if err.0 == 0 {
        Ok(())
    } else if err == ERROR_ACCESS_DENIED || err == ERROR_PRIVILEGE_NOT_HELD {
        Err(BackendError::AccessDenied(format!("{what} (código {})", err.0)))
    } else {
        Err(BackendError::Failed(format!("{what} (código {})", err.0)))
    }
}

fn wmi_err(err: wmi::WMIError) -> BackendError {
    let raw = err.to_string();
    let code = hresult_code(&raw);
    let pt = match code {
        Some(0x80041017) => "consulta WMI inválida para essa classe (0x80041017)".to_string(),
        Some(0x80041003) => "acesso negado pelo WMI (0x80041003)".to_string(),
        Some(0x80041002) => "objeto WMI não encontrado (0x80041002)".to_string(),
        Some(0x80041010) => "classe WMI inexistente neste Windows (0x80041010)".to_string(),
        Some(0x80070005) => "acesso negado (0x80070005)".to_string(),
        Some(code) => format!("falha do Windows 0x{code:08X}"),
        None => raw.replace("HRESULT Call failed with: ", "falha do Windows: "),
    };
    let denied = matches!(code, Some(0x80041003) | Some(0x80070005))
        || raw.to_ascii_lowercase().contains("access")
        || raw.to_ascii_lowercase().contains("privilege");
    if denied {
        BackendError::AccessDenied(pt)
    } else {
        BackendError::Failed(pt)
    }
}

fn hresult_code(text: &str) -> Option<u32> {
    let lower = text.to_ascii_lowercase();
    let at = lower.find("0x")?;
    let hex: String = lower[at + 2..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    u32::from_str_radix(&hex, 16).ok()
}

fn wmi_query<T: serde::de::DeserializeOwned>(backend: &WindowsBackend, namespace: &str, what: &str, query: &str) -> Result<Vec<T>, BackendError> {
    let wmi = match backend.wmi(namespace) {
        Ok(wmi) => wmi,
        Err(err) => {
            crate::store::trace(&format!("wmi {what} namespace={namespace} conexão falhou: {err}"));
            return Err(err);
        }
    };
    match wmi.raw_query(query) {
        Ok(rows) => {
            crate::store::trace(&format!("wmi ok {what} {namespace} :: {query}"));
            Ok(rows)
        }
        Err(err) => {
            let mapped = wmi_err(err);
            crate::store::trace(&format!("wmi falha {what} {namespace} :: {query} :: {mapped}"));
            Err(mapped)
        }
    }
}

fn open_key(hive: Hive, path: &str, access: u32) -> Result<Option<RegHandle>, BackendError> {
    let mut key = HKEY::default();
    let path_w = wide(path);
    let err = unsafe { RegOpenKeyExW(hive_key(hive), PCWSTR(path_w.as_ptr()), 0, windows::Win32::System::Registry::REG_SAM_FLAGS(access), &mut key) };
    if err == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    win_ok(err, "abrir chave do registro")?;
    Ok(Some(RegHandle(key)))
}

fn create_key(hive: Hive, path: &str) -> Result<RegHandle, BackendError> {
    let mut key = HKEY::default();
    let path_w = wide(path);
    let err = unsafe {
        RegCreateKeyExW(hive_key(hive), PCWSTR(path_w.as_ptr()), 0, None, windows::Win32::System::Registry::REG_OPEN_CREATE_OPTIONS(0), windows::Win32::System::Registry::REG_SAM_FLAGS(KEY_READ.0 | KEY_SET_VALUE.0), None, &mut key, None)
    };
    win_ok(err, "criar chave do registro")?;
    Ok(RegHandle(key))
}

fn query_value(key: HKEY, name: &str) -> Result<Option<RegValue>, BackendError> {
    let name_w = wide(name);
    let mut typ = REG_VALUE_TYPE(0);
    let mut size = 0u32;
    let err = unsafe { RegQueryValueExW(key, PCWSTR(name_w.as_ptr()), None, Some(&mut typ), None, Some(&mut size)) };
    if err == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    win_ok(err, "ler tamanho do valor")?;
    let mut buf = vec![0u8; size as usize];
    let err = unsafe { RegQueryValueExW(key, PCWSTR(name_w.as_ptr()), None, Some(&mut typ), Some(buf.as_mut_ptr()), Some(&mut size)) };
    win_ok(err, "ler valor do registro")?;
    buf.truncate(size as usize);
    Ok(Some(decode_reg(typ, &buf)))
}

fn decode_reg(typ: REG_VALUE_TYPE, buf: &[u8]) -> RegValue {
    if typ == REG_DWORD && buf.len() >= 4 {
        return RegValue::Dword(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]));
    }
    if typ == REG_SZ || typ == REG_EXPAND_SZ {
        let words: Vec<u16> = buf.chunks(2).filter_map(|c| if c.len() == 2 { Some(u16::from_le_bytes([c[0], c[1]])) } else { None }).collect();
        let end = words.iter().position(|c| *c == 0).unwrap_or(words.len());
        return RegValue::Sz(String::from_utf16_lossy(&words[..end]));
    }
    if typ == REG_MULTI_SZ {
        let words: Vec<u16> = buf.chunks(2).filter_map(|c| if c.len() == 2 { Some(u16::from_le_bytes([c[0], c[1]])) } else { None }).collect();
        let text = String::from_utf16_lossy(&words);
        let parts = text.split('\0').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
        return RegValue::MultiSz(parts);
    }
    RegValue::Binary(buf.to_vec())
}

fn set_value(key: HKEY, name: &str, value: &RegValue) -> Result<(), BackendError> {
    let name_w = wide(name);
    let (typ, bytes) = match value {
        RegValue::Dword(v) => (REG_DWORD, v.to_le_bytes().to_vec()),
        RegValue::Sz(s) => {
            let mut bytes = Vec::new();
            for unit in wide(s) {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            (REG_SZ, bytes)
        }
        RegValue::Binary(b) => (REG_BINARY, b.clone()),
        RegValue::MultiSz(parts) => {
            let mut words = Vec::new();
            for part in parts {
                words.extend(wide(part));
            }
            words.push(0);
            let mut bytes = Vec::new();
            for unit in words {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            (REG_MULTI_SZ, bytes)
        }
    };
    let err = unsafe { RegSetValueExW(key, PCWSTR(name_w.as_ptr()), 0, typ, Some(&bytes)) };
    win_ok(err, "gravar valor do registro")
}

fn open_service(name: &str, access: u32) -> Result<SvcHandle, BackendError> {
    let name_w = wide(name);
    unsafe {
        let scm = OpenSCManagerW(None, None, SC_MANAGER_CONNECT).map_err(|e| BackendError::Failed(format!("abrir o gerenciador de serviços: {e}")))?;
        let scm = SvcHandle(scm);
        let svc = OpenServiceW(scm.0, PCWSTR(name_w.as_ptr()), access).map_err(|e| {
            let msg = e.to_string();
            let code = e.code().0 as u32;
            if code == 0x8007_0424 || (code & 0xFFFF) == 1060 {
                BackendError::NotFound(format!("serviço {name} não existe"))
            } else if msg.to_ascii_lowercase().contains("access") {
                BackendError::AccessDenied(format!("serviço {name}: {msg}"))
            } else {
                BackendError::Failed(format!("abrir serviço {name}: {msg}"))
            }
        })?;
        Ok(SvcHandle(svc))
    }
}

fn SERVICE_CHANGE_CONFIG() -> u32 {
    0x0002
}
fn SERVICE_STOP_RIGHT() -> u32 {
    0x0020
}

fn spi_get(action: windows::Win32::UI::WindowsAndMessaging::SYSTEM_PARAMETERS_INFO_ACTION, param: u32, ptr: *mut core::ffi::c_void) -> Result<(), BackendError> {
    unsafe { SystemParametersInfoW(action, param, Some(ptr), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).map_err(|e| BackendError::Failed(e.to_string())) }
}

fn spi_set(action: windows::Win32::UI::WindowsAndMessaging::SYSTEM_PARAMETERS_INFO_ACTION, param: u32, ptr: *mut core::ffi::c_void) -> Result<(), BackendError> {
    let flags = SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(SPIF_UPDATEINIFILE.0 | SPIF_SENDCHANGE.0);
    unsafe { SystemParametersInfoW(action, param, Some(ptr), flags).map_err(|e| BackendError::Failed(e.to_string())) }
}

const POWER_SUB_PERSONALITY: &str = "fea3413e-7e05-4911-9a71-700331f1c294";
const POWER_PERSONALITY: &str = "245d8541-3943-4422-b025-13a784f679b7";
const POWER_SUB_DISK: &str = "0012ee47-9041-4b5d-9b77-535fba8b1442";
const POWER_DISK_TIMEOUT: &str = "6738e2c4-e8a5-4a42-b16a-e040e769756e";
const POWER_SUB_PROCESSOR: &str = "54533251-82be-4824-96c1-47b60b740d00";
const POWER_MIN_PROCESSOR: &str = "893dee8e-2bef-41e0-89c6-b55d0929964c";

fn power_status() -> Option<SYSTEM_POWER_STATUS> {
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut status).ok()? };
    Some(status)
}

fn read_power_schemes() -> Result<Vec<PowerSchemeInfo>, BackendError> {
    let mut out = Vec::new();
    let mut index = 0u32;
    loop {
        let mut buf = [0u8; std::mem::size_of::<GUID>()];
        let mut size = buf.len() as u32;
        let err = unsafe { PowerEnumerate(HKEY::default(), None, None, ACCESS_SCHEME, index, Some(buf.as_mut_ptr()), &mut size) };
        if err.0 == 259 {
            break;
        }
        win_ok(err, "enumerar planos de energia")?;
        let guid = unsafe { *(buf.as_ptr() as *const GUID) };
        let text = guid_to_string(&guid);
        out.push(PowerSchemeInfo {
            name: power_friendly_name(&guid),
            personality: power_ac_index(&guid, POWER_SUB_PERSONALITY, POWER_PERSONALITY),
            disk_timeout_ac: power_ac_index(&guid, POWER_SUB_DISK, POWER_DISK_TIMEOUT),
            min_processor_ac: power_ac_index(&guid, POWER_SUB_PROCESSOR, POWER_MIN_PROCESSOR),
            guid: text,
        });
        index += 1;
        if index > 64 {
            break;
        }
    }
    Ok(out)
}

fn power_friendly_name(guid: &GUID) -> String {
    let mut size = 0u32;
    let probe = unsafe { PowerReadFriendlyName(HKEY::default(), Some(guid), None, None, None, &mut size) };
    if probe.0 != 0 && probe != ERROR_MORE_DATA || size < 4 {
        return String::new();
    }
    let mut buf = vec![0u8; size as usize];
    let err = unsafe { PowerReadFriendlyName(HKEY::default(), Some(guid), None, None, Some(buf.as_mut_ptr()), &mut size) };
    if err.0 != 0 {
        return String::new();
    }
    let words = size as usize / 2;
    let units = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u16, words.min(buf.len() / 2)) };
    utf16_fixed(units).trim().to_string()
}

fn power_ac_index(scheme: &GUID, subgroup: &str, setting: &str) -> Option<u32> {
    let subgroup = parse_guid(subgroup).ok()?;
    let setting = parse_guid(setting).ok()?;
    let mut value = 0u32;
    let err = unsafe { PowerReadACValueIndex(HKEY::default(), Some(scheme), Some(&subgroup), Some(&setting), &mut value) };
    if err.0 == 0 { Some(value) } else { None }
}

/// CPU, GPU, RAM e monitor só para o painel. Não entra em nenhum ajuste.
pub fn hardware_summary() -> (String, String, String, String) {
    let cpu = reg_sz(Hive::Hklm, r"HARDWARE\DESCRIPTION\System\CentralProcessor\0", "ProcessorNameString")
        .unwrap_or_else(|| "CPU não identificada".into());
    let gpu = gpu_name().unwrap_or_else(|| "GPU não identificada".into());
    let ram = ram_label().unwrap_or_else(|| "RAM não identificada".into());
    let monitor = monitor_label().unwrap_or_else(|| "Monitor não identificado".into());
    (cpu, gpu, ram, monitor)
}

fn reg_sz(hive: Hive, path: &str, name: &str) -> Option<String> {
    let key = open_key(hive, path, KEY_READ.0).ok()??;
    query_value(key.0, name).ok().flatten().and_then(|value| match value {
        RegValue::Sz(text) => Some(text.trim().to_string()),
        _ => None,
    })
}

fn gpu_name() -> Option<String> {
    let class = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let root = open_key(Hive::Hklm, class, KEY_READ.0).ok()??;
    let mut names = Vec::new();
    for sub in enum_subkeys(root.0) {
        if !sub.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let path = format!(r"{class}\{sub}");
        if let Some(desc) = reg_sz(Hive::Hklm, &path, "DriverDesc") {
            let lower = desc.to_ascii_lowercase();
            if lower.contains("microsoft basic") || lower.contains("remote") {
                continue;
            }
            names.push(desc);
        }
    }
    names.into_iter().next()
}

fn ram_label() -> Option<String> {
    let mut status = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe { GlobalMemoryStatusEx(&mut status).ok()? };
    let gb = status.ullTotalPhys as f64 / 1024.0 / 1024.0 / 1024.0;
    Some(format!("{gb:.0} GB"))
}

fn monitor_label() -> Option<String> {
    let monitors = read_monitors().ok()?;
    let first = monitors.first()?;
    let name = if first.device.is_empty() { "Monitor".to_string() } else { first.device.clone() };
    Some(format!("{name} · {} Hz", first.hz))
}

fn guid_to_string(guid: &GUID) -> String {
    format!("{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}", guid.data1, guid.data2, guid.data3, guid.data4[0], guid.data4[1], guid.data4[2], guid.data4[3], guid.data4[4], guid.data4[5], guid.data4[6], guid.data4[7])
}

fn parse_guid(text: &str) -> Result<GUID, BackendError> {
    let bare = normalize_guid(text).replace('-', "");
    if bare.len() != 32 || !bare.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(BackendError::Failed(format!("GUID inválido: {text}")));
    }
    let data1 = u32::from_str_radix(&bare[0..8], 16).unwrap_or(0);
    let data2 = u16::from_str_radix(&bare[8..12], 16).unwrap_or(0);
    let data3 = u16::from_str_radix(&bare[12..16], 16).unwrap_or(0);
    let mut data4 = [0u8; 8];
    for (i, byte) in data4.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&bare[16 + i * 2..18 + i * 2], 16).unwrap_or(0);
    }
    Ok(GUID::from_values(data1, data2, data3, data4))
}

fn utf16_fixed(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

fn read_devmode(device: PCWSTR, mode: ENUM_DISPLAY_SETTINGS_MODE) -> Option<DEVMODEW> {
    let mut dm = DEVMODEW::default();
    dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    let ok = unsafe { EnumDisplaySettingsW(device, mode, &mut dm) };
    if ok.as_bool() { Some(dm) } else { None }
}

fn read_monitors() -> Result<Vec<MonitorMode>, BackendError> {
    let mut monitors = Vec::new();
    for index in 0..16 {
        let mut device = DISPLAY_DEVICEW::default();
        device.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
        let ok = unsafe { EnumDisplayDevicesW(PCWSTR::null(), index, &mut device, 0) };
        if !ok.as_bool() {
            break;
        }
        if device.StateFlags & 1 == 0 {
            continue;
        }
        let name = utf16_fixed(&device.DeviceName);
        let label = utf16_fixed(&device.DeviceString);
        let name_h = HSTRING::from(&name);
        let current = read_devmode(PCWSTR(name_h.as_ptr()), ENUM_CURRENT_SETTINGS);
        let Some(current) = current else { continue };
        let mut max_hz = current.dmDisplayFrequency;
        for mode_index in 0..400 {
            let Some(mode) = read_devmode(PCWSTR(name_h.as_ptr()), ENUM_DISPLAY_SETTINGS_MODE(mode_index)) else { break };
            if mode.dmPelsWidth == current.dmPelsWidth && mode.dmPelsHeight == current.dmPelsHeight {
                max_hz = max_hz.max(mode.dmDisplayFrequency);
            }
        }
        monitors.push(MonitorMode {
            device: name,
            label,
            width: current.dmPelsWidth,
            height: current.dmPelsHeight,
            hz: current.dmDisplayFrequency,
            max_hz,
        });
    }
    Ok(monitors)
}

fn read_disks(backend: &WindowsBackend) -> Result<Vec<DiskHealth>, BackendError> {
    #[derive(Deserialize)]
    struct DiskRow {
        FriendlyName: Option<String>,
        MediaType: Option<u16>,
        BusType: Option<u16>,
        HealthStatus: Option<u16>,
        DeviceId: Option<String>,
    }
    #[derive(Deserialize)]
    struct CounterRow {
        DeviceId: Option<String>,
        Temperature: Option<u16>,
        TemperatureMax: Option<u16>,
        Wear: Option<u8>,
        PowerOnHours: Option<u64>,
        ReadErrorsUncorrected: Option<u64>,
        WriteErrorsUncorrected: Option<u64>,
    }
    let disks: Vec<DiskRow> = wmi_query(backend, r"root\Microsoft\Windows\Storage", "discos físicos", "SELECT FriendlyName, MediaType, BusType, HealthStatus, DeviceId FROM MSFT_PhysicalDisk")?;
    let counters: Vec<CounterRow> = wmi_query(
        backend,
        r"root\Microsoft\Windows\Storage",
        "contadores SMART",
        "SELECT DeviceId, Temperature, TemperatureMax, Wear, PowerOnHours, ReadErrorsUncorrected, WriteErrorsUncorrected FROM MSFT_StorageReliabilityCounter",
    )
    .unwrap_or_default();
    let mut by_id: BTreeMap<String, &CounterRow> = BTreeMap::new();
    for counter in &counters {
        if let Some(id) = &counter.DeviceId {
            by_id.insert(id.clone(), counter);
        }
    }
    let mut out = Vec::new();
    for disk in disks {
        let counter = disk.DeviceId.as_ref().and_then(|id| by_id.get(id).copied());
        let bus = disk.BusType.unwrap_or(0);
        let media = match (disk.MediaType.unwrap_or(0), bus) {
            (_, 17) => MediaKind::Nvme,
            (3, _) => MediaKind::Hdd,
            (4 | 5, _) => MediaKind::Ssd,
            _ => MediaKind::Unknown,
        };
        let health = match disk.HealthStatus.unwrap_or(5) {
            0 => "Healthy",
            1 => "Warning",
            2 => "Unhealthy",
            _ => "Unknown",
        };
        out.push(DiskHealth {
            name: disk.FriendlyName.unwrap_or_else(|| "Disco".into()),
            media,
            bus: bus_name(bus),
            health: health.into(),
            temp_c: counter.and_then(|c| c.Temperature.map(|v| v as i32)),
            temp_max_c: counter.and_then(|c| c.TemperatureMax.map(|v| v as i32)),
            wear: counter.and_then(|c| c.Wear.map(|v| v as i32)),
            power_on_hours: counter.and_then(|c| c.PowerOnHours.map(|v| v as i64)),
            read_errors: counter.and_then(|c| c.ReadErrorsUncorrected.map(|v| v as i64)),
            write_errors: counter.and_then(|c| c.WriteErrorsUncorrected.map(|v| v as i64)),
        });
    }
    Ok(out)
}

fn bus_name(bus: u16) -> String {
    match bus {
        17 => "NVMe".into(),
        11 => "SATA".into(),
        7 => "USB".into(),
        8 => "SD".into(),
        10 => "SAS".into(),
        _ => format!("bus {bus}"),
    }
}

fn read_disk_events() -> Result<Vec<DiskEventGroup>, BackendError> {
    let query = HSTRING::from("*[System[(EventID=7 or EventID=51 or EventID=129 or EventID=153) and TimeCreated[timediff(@SystemTime) <= 604800000]]]");
    let handle = unsafe { EvtQuery(None, w!("System"), &query, EvtQueryReverseDirection.0 | 1).map_err(|e| BackendError::Failed(format!("ler o log do Sistema: {e}")))? };
    let mut groups: BTreeMap<(u32, String), (u32, String)> = BTreeMap::new();
    let mut batch = [0isize; 16];
    for _ in 0..40 {
        let mut returned = 0u32;
        let next = unsafe { EvtNext(handle, &mut batch, 2000, 0, &mut returned) };
        if next.is_err() || returned == 0 {
            break;
        }
        for event in batch.iter().take(returned as usize) {
            if let Some((id, provider, sample)) = render_event(*event) {
                let slot = groups.entry((id, provider)).or_insert((0, sample));
                slot.0 += 1;
            }
            unsafe { let _ = EvtClose(EVT_HANDLE(*event)); }
        }
    }
    unsafe { let _ = EvtClose(handle); }
    let mut out: Vec<_> = groups.into_iter().map(|((id, provider), (count, sample))| DiskEventGroup { id, provider, count, sample }).collect();
    out.sort_by_key(|g| std::cmp::Reverse(g.count));
    Ok(out)
}

fn render_event(event: isize) -> Option<(u32, String, String)> {
    let mut used = 0u32;
    let mut count = 0u32;
    unsafe { let _ = EvtRender(None, EVT_HANDLE(event), EvtRenderEventXml.0, 0, None, &mut used, &mut count); }
    if used == 0 {
        return None;
    }
    let mut buf = vec![0u8; used as usize + 4];
    let ok = unsafe { EvtRender(None, EVT_HANDLE(event), EvtRenderEventXml.0, buf.len() as u32, Some(buf.as_mut_ptr() as *mut _), &mut used, &mut count) };
    if ok.is_err() {
        return None;
    }
    let words = buf.len() / 2;
    let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u16, words) });
    let provider = attr(&text, "Provider", "Name").unwrap_or_else(|| "System".into());
    let id = between(&text, "<EventID>", "</EventID").or_else(|| between(&text, "<EventID ", "</EventID")).unwrap_or_default();
    let id = id.split('>').next_back().unwrap_or("").trim().parse::<u32>().ok()?;
    let sample = between(&text, "<EventData>", "</EventData>").unwrap_or_default();
    let sample = strip_tags(&sample);
    let sample = sample.chars().take(180).collect::<String>();
    Some((id, provider, sample))
}

fn attr(xml: &str, tag: &str, name: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag} "))?;
    let end = xml[start..].find('>')? + start;
    let chunk = &xml[start..end];
    let key = format!("{name}=\"");
    let at = chunk.find(&key)? + key.len();
    let rest = &chunk[at..];
    let to = rest.find('"')?;
    Some(rest[..to].to_string())
}

fn between(text: &str, open: &str, close: &str) -> Option<String> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(text[start..end].trim().to_string())
}

fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for ch in text.chars() {
        match ch {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sample_disk(backend: &WindowsBackend) -> Result<DiskReport, BackendError> {
    let mut query = 0isize;
    let status = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) };
    if status != 0 {
        return Err(BackendError::Failed(format!("abrir contadores de desempenho ({status})")));
    }
    let mut idle = 0isize;
    let mut queue = 0isize;
    let mut latency = 0isize;
    let mut io = 0isize;
    let mut pid = 0isize;
    unsafe {
        add_counter(query, r"\PhysicalDisk(*)\% Idle Time", &mut idle)?;
        add_counter(query, r"\PhysicalDisk(*)\Avg. Disk sec/Transfer", &mut latency)?;
        add_counter(query, r"\PhysicalDisk(*)\Current Disk Queue Length", &mut queue)?;
        add_counter(query, r"\Process(*)\IO Data Bytes/sec", &mut io)?;
        add_counter(query, r"\Process(*)\ID Process", &mut pid)?;
        let _ = PdhCollectQueryData(query);
    }
    let mut idle_sum: BTreeMap<String, f64> = BTreeMap::new();
    let mut lat_sum: BTreeMap<String, f64> = BTreeMap::new();
    let mut queue_sum: BTreeMap<String, f64> = BTreeMap::new();
    let mut io_sum: BTreeMap<String, f64> = BTreeMap::new();
    let mut samples: f64 = 0.0;
    for _ in 0..10 {
        thread::sleep(Duration::from_secs(1));
        let status = unsafe { PdhCollectQueryData(query) };
        if status != 0 {
            continue;
        }
        samples += 1.0;
        for (name, value) in counter_array(idle) {
            *idle_sum.entry(name).or_default() += value;
        }
        for (name, value) in counter_array(latency) {
            *lat_sum.entry(name).or_default() += value;
        }
        for (name, value) in counter_array(queue) {
            *queue_sum.entry(name).or_default() += value;
        }
        for (name, value) in counter_array(io) {
            *io_sum.entry(name).or_default() += value;
        }
    }
    let pids = counter_array(pid);
    unsafe { let _ = PdhCloseQuery(query); }
    let div = samples.max(1.0);
    let media = disk_media_by_index(backend);
    let mut disks = Vec::new();
    let mut hdd_busy = 0.0f32;
    for (name, idle_v) in &idle_sum {
        if name == "_Total" {
            continue;
        }
        let busy = (100.0 - (idle_v / div)).clamp(0.0, 100.0) as f32;
        let index = name.split_whitespace().next().and_then(|n| n.parse::<u32>().ok());
        let kind = index.and_then(|i| media.get(&i).copied()).unwrap_or(MediaKind::Unknown);
        if kind == MediaKind::Hdd {
            hdd_busy = hdd_busy.max(busy);
        }
        disks.push(DiskActivity {
            name: name.clone(),
            media: kind,
            busy_percent: busy,
            queue: queue_sum.get(name).copied().unwrap_or(0.0) as f32 / div as f32,
            sec_per_transfer: (lat_sum.get(name).copied().unwrap_or(0.0) / div) as f32,
        });
    }
    let mut processes: Vec<ProcessIo> = io_sum
        .into_iter()
        .filter(|(name, _)| name != "_Total" && name != "Idle")
        .map(|(name, value)| ProcessIo { name, bytes_per_sec: value / div })
        .collect();
    processes.sort_by(|a, b| b.bytes_per_sec.partial_cmp(&a.bytes_per_sec).unwrap_or(std::cmp::Ordering::Equal));
    processes.truncate(5);
    let sysmain_pid = service_pid("SysMain").unwrap_or(0);
    let sysmain_name = pids.iter().find(|(_, id)| *id as u32 == sysmain_pid).map(|(name, _)| name.clone());
    let top_name = processes.first().map(|p| p.name.clone());
    let sysmain_is_top_on_hdd = sysmain_pid != 0 && hdd_busy >= 20.0 && sysmain_name.is_some() && sysmain_name == top_name;
    Ok(DiskReport { disks, top_processes: processes, sysmain_is_top_on_hdd })
}

fn add_counter(query: isize, path: &str, out: &mut isize) -> Result<(), BackendError> {
    let text = HSTRING::from(path);
    let status = unsafe { PdhAddEnglishCounterW(query, &text, 0, out) };
    if status == 0 {
        Ok(())
    } else {
        Err(BackendError::Failed(format!("contador {path} ({status})")))
    }
}

fn counter_array(counter: isize) -> Vec<(String, f64)> {
    let mut size = 0u32;
    let mut count = 0u32;
    unsafe { let _ = PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None); }
    if size == 0 {
        return Vec::new();
    }
    let mut buf = vec![0u8; size as usize + 64];
    let status = unsafe { PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W)) };
    if status != 0 || count == 0 {
        return Vec::new();
    }
    let items = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const PDH_FMT_COUNTERVALUE_ITEM_W, count as usize) };
    items.iter().filter_map(|item| {
        let name = utf16_z(item.szName.0);
        let value = unsafe { item.FmtValue.Anonymous.doubleValue };
        if name.is_empty() { None } else { Some((name, value)) }
    }).collect()
}

fn utf16_z(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe {
        let mut len = 0;
        while *ptr.add(len) != 0 && len < 1024 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
    }
}

fn service_pid(name: &str) -> Option<u32> {
    let handle = open_service(name, SERVICE_QUERY_STATUS).ok()?;
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32;
    unsafe {
        QueryServiceStatusEx(handle.0, SC_STATUS_PROCESS_INFO, Some(std::slice::from_raw_parts_mut(&mut status as *mut _ as *mut u8, needed as usize)), &mut needed).ok()?;
    }
    Some(status.dwProcessId)
}

fn disk_media_by_index(backend: &WindowsBackend) -> BTreeMap<u32, MediaKind> {
    let mut map = BTreeMap::new();
    if let Ok(disks) = read_disks(backend) {
        for (index, disk) in disks.iter().enumerate() {
            map.insert(index as u32, disk.media);
        }
    }
    map
}

fn read_location(backend: &WindowsBackend) -> Result<WindowsLocation, BackendError> {
    let mut buf = [0u16; 260];
    let n = unsafe { GetWindowsDirectoryW(Some(&mut buf)) };
    if n == 0 {
        return Err(BackendError::Failed("não achei a pasta do Windows".into()));
    }
    let windows = String::from_utf16_lossy(&buf[..n as usize]);
    let system_drive = windows.chars().next().map(|c| format!("{c}:")).unwrap_or_else(|| "C:".into());
    let page = backend.reg_get(Hive::Hklm, r"SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management", "PagingFiles")?;
    let (page_path, page_mb) = match page {
        Some(RegValue::MultiSz(lines)) => parse_pagefile(lines.first().map(String::as_str).unwrap_or("")),
        Some(RegValue::Sz(line)) => parse_pagefile(&line),
        _ => (format!(r"{system_drive}\pagefile.sys"), 0),
    };
    #[derive(Deserialize)]
    struct Link { Antecedent: Option<String>, Dependent: Option<String> }
    #[derive(Deserialize)]
    struct Page { Name: Option<String>, AllocatedBaseSize: Option<u64> }
    let links: Vec<Link> = wmi_query(backend, r"root\cimv2", "disco do Windows", "SELECT Antecedent, Dependent FROM Win32_LogicalDiskToPartition")?;
    let mut letter_disk: BTreeMap<String, u32> = BTreeMap::new();
    for link in links {
        let Some(dep) = link.Dependent else { continue };
        let Some(ant) = link.Antecedent else { continue };
        if let (Some(letter), Some(index)) = (drive_letter(&dep), disk_index(&ant)) {
            letter_disk.insert(letter.to_ascii_uppercase(), index);
        }
    }
    let usage: Vec<Page> = wmi_query(backend, r"root\cimv2", "arquivo de paginação", "SELECT Name, AllocatedBaseSize FROM Win32_PageFileUsage").unwrap_or_default();
    let page_mb = usage.first().and_then(|p| p.AllocatedBaseSize).unwrap_or(page_mb);
    let page_path = usage.first().and_then(|p| p.Name.clone()).unwrap_or(page_path);
    let media = disk_media_by_index(backend);
    let system_index = letter_disk.get(&system_drive.to_ascii_uppercase()).copied();
    let page_letter = page_path.chars().next().map(|c| format!("{c}:")).unwrap_or_else(|| system_drive.clone());
    let page_index = letter_disk.get(&page_letter.to_ascii_uppercase()).copied().or(system_index);
    Ok(WindowsLocation {
        system_drive: system_drive.clone(),
        system_disk: system_index.map(|i| format!("disco {i}")).unwrap_or_else(|| "disco do Windows".into()),
        system_media: system_index.and_then(|i| media.get(&i).copied()).unwrap_or(MediaKind::Unknown),
        pagefile_path: page_path,
        pagefile_size_mb: page_mb,
        pagefile_media: page_index.and_then(|i| media.get(&i).copied()).unwrap_or(MediaKind::Unknown),
    })
}

fn parse_pagefile(line: &str) -> (String, u64) {
    let mut parts = line.split_whitespace();
    let path = parts.next().unwrap_or(r"C:\pagefile.sys").to_string();
    let mb = parts.next_back().and_then(|n| n.parse().ok()).unwrap_or(0);
    (path, mb)
}

fn drive_letter(text: &str) -> Option<String> {
    let key = "DeviceID=\"";
    let at = text.find(key)? + key.len();
    let ch = text[at..].chars().next()?;
    Some(format!("{ch}:"))
}

fn disk_index(text: &str) -> Option<u32> {
    let key = "Disk #";
    let at = text.find(key)? + key.len();
    let num: String = text[at..].chars().take_while(|c| c.is_ascii_digit()).collect();
    num.parse().ok()
}

fn read_adapters(backend: &WindowsBackend) -> Result<Vec<AdapterInfo>, BackendError> {
    // MSFT_NetAdapter não tem LinkSpeed. No CIM existem Speed, ReceiveLinkSpeed e TransmitLinkSpeed.
    #[derive(Deserialize)]
    struct Row {
        Name: Option<String>,
        InterfaceDescription: Option<String>,
        InterfaceGuid: Option<String>,
        NdisPhysicalMedium: Option<u32>,
        HardwareInterface: Option<bool>,
        MediaConnectState: Option<u32>,
        Speed: Option<u64>,
        ReceiveLinkSpeed: Option<u64>,
        TransmitLinkSpeed: Option<u64>,
    }
    let with_guid = "SELECT Name, InterfaceDescription, InterfaceGuid, NdisPhysicalMedium, HardwareInterface, MediaConnectState, Speed, ReceiveLinkSpeed, TransmitLinkSpeed FROM MSFT_NetAdapter";
    let rows: Vec<Row> = match wmi_query(backend, r"root\StandardCimv2", "placas de rede", with_guid) {
        Ok(rows) => rows,
        Err(err) => {
            crate::store::trace(&format!("n-link consulta com InterfaceGuid falhou ({err}); tentando sem o campo"));
            let fallback = "SELECT Name, InterfaceDescription, NdisPhysicalMedium, HardwareInterface, MediaConnectState, Speed, ReceiveLinkSpeed, TransmitLinkSpeed FROM MSFT_NetAdapter";
            wmi_query(backend, r"root\StandardCimv2", "placas de rede", fallback)?
        }
    };
    let mut out = Vec::new();
    for row in rows {
        if row.HardwareInterface == Some(false) {
            continue;
        }
        let name = row.Name.clone().unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let description = row.InterfaceDescription.clone().unwrap_or_default();
        let medium = row.NdisPhysicalMedium.unwrap_or(0);
        let wired = nic_is_wired(medium, &description);
        let connected = row.MediaConnectState == Some(1);
        let guid = row.InterfaceGuid.clone().unwrap_or_default();
        let props = nic_props(backend, &name, &guid);
        let link = row.ReceiveLinkSpeed.unwrap_or(0).max(row.TransmitLinkSpeed.unwrap_or(0)).max(row.Speed.unwrap_or(0));
        let max_bps = crate::rules::inferred_link_cap_bps(&description).unwrap_or(link);
        out.push(AdapterInfo {
            name,
            interface_guid: guid,
            description,
            wired,
            connected,
            link_bps: link,
            max_bps,
            power_props: props,
        });
    }
    Ok(out)
}

fn nic_is_wired(medium: u32, description: &str) -> bool {
    // 14 = 802.3, 9 = 802.11, 10 = Bluetooth. O nome da conexão não entra.
    match medium {
        14 => true,
        9 | 10 => false,
        _ => {
            let text = description.to_ascii_lowercase();
            let wireless = text.contains("wi-fi") || text.contains("wifi") || text.contains("wireless") || text.contains("wlan") || text.contains("802.11") || text.contains("bluetooth");
            !wireless && (text.contains("ethernet") || text.contains("gbe") || text.contains("i225") || text.contains("i226"))
        }
    }
}

fn nic_props(backend: &WindowsBackend, adapter: &str, interface_guid: &str) -> Vec<NicPowerProp> {
    let safe: String = adapter.chars().filter(|c| *c != '\'' && *c != ';' && *c != '"').collect();
    let from_cim = if safe.is_empty() { Vec::new() } else { cim_nic_props(backend, &safe) };
    if !from_cim.is_empty() {
        return from_cim;
    }
    registry_nic_props(backend, interface_guid, adapter)
}

fn cim_nic_props(backend: &WindowsBackend, adapter: &str) -> Vec<NicPowerProp> {
    #[derive(Deserialize, Default)]
    struct Prop {
        DisplayName: Option<String>,
        RegistryKeyword: Option<String>,
        DisplayValue: Option<String>,
        RegistryValue: Option<Vec<String>>,
    }
    let query = format!("SELECT Name, DisplayName, RegistryKeyword, DisplayValue, RegistryValue FROM MSFT_NetAdapterAdvancedPropertySettingData WHERE Name = '{adapter}'");
    let rows: Vec<Prop> = match wmi_query(backend, r"root\StandardCimv2", "propriedades da placa", &query) {
        Ok(rows) => rows,
        Err(err) => {
            crate::store::trace(&format!("n-eee CIM leitura de {adapter} falhou: {err}"));
            return Vec::new();
        }
    };
    rows.into_iter().filter_map(|row| {
        let keyword = row.RegistryKeyword.unwrap_or_default();
        if !crate::rules::is_nic_power_keyword(&keyword) {
            return None;
        }
        let current = row.RegistryValue.and_then(|v| v.into_iter().next()).unwrap_or_default();
        Some(NicPowerProp {
            display_name: row.DisplayName.unwrap_or_else(|| keyword.clone()),
            keyword,
            current,
            current_display: row.DisplayValue.unwrap_or_default(),
            disabled_value: "0".into(),
            disabled_display: "0".into(),
        })
    }).collect()
}

fn registry_nic_props(backend: &WindowsBackend, interface_guid: &str, adapter: &str) -> Vec<NicPowerProp> {
    let Some(sub) = find_nic_subkey(backend, interface_guid, adapter) else { return Vec::new(); };
    let path = format!(r"{NET_CLASS}\{sub}");
    let Ok(values) = backend.reg_list(Hive::Hklm, &path) else { return Vec::new(); };
    let mut out = Vec::new();
    for (name, value) in values {
        if !crate::rules::is_nic_power_keyword(&name) {
            continue;
        }
        let current = match value {
            RegValue::Sz(text) => text,
            RegValue::Dword(n) => n.to_string(),
            _ => continue,
        };
        out.push(NicPowerProp {
            display_name: name.clone(),
            keyword: name,
            current_display: current.clone(),
            disabled_value: "0".into(),
            disabled_display: "0".into(),
            current,
        });
    }
    out
}

fn find_nic_subkey(backend: &WindowsBackend, interface_guid: &str, connection_name: &str) -> Option<String> {
    let root = open_key(Hive::Hklm, NET_CLASS, KEY_READ.0).ok()??;
    let subs = enum_subkeys(root.0);
    let mut seen = Vec::new();
    for sub in &subs {
        let path = format!(r"{NET_CLASS}\{sub}");
        let netcfg = match backend.reg_get(Hive::Hklm, &path, "NetCfgInstanceId").ok().flatten() {
            Some(RegValue::Sz(text)) => text,
            _ => String::new(),
        };
        seen.push(format!("{sub}={netcfg}"));
        if !interface_guid.is_empty() && crate::rules::guids_equal(&netcfg, interface_guid) {
            return Some(sub.clone());
        }
    }
    if let Some(guid) = connection_guid(backend, connection_name) {
        for sub in &subs {
            let path = format!(r"{NET_CLASS}\{sub}");
            if let Some(RegValue::Sz(text)) = backend.reg_get(Hive::Hklm, &path, "NetCfgInstanceId").ok().flatten() {
                if crate::rules::guids_equal(&text, &guid) {
                    return Some(sub.clone());
                }
            }
        }
    }
    crate::store::trace(&format!(
        "n-eee não achei NetCfgInstanceId {interface_guid} (conexão {connection_name}). subchaves: {}",
        seen.join(", ")
    ));
    None
}

fn connection_guid(backend: &WindowsBackend, connection_name: &str) -> Option<String> {
    if connection_name.is_empty() {
        return None;
    }
    let root_path = format!(r"SYSTEM\CurrentControlSet\Control\Network\{{4d36e972-e325-11ce-bfc1-08002be10318}}");
    let root = open_key(Hive::Hklm, &root_path, KEY_READ.0).ok()??;
    for sub in enum_subkeys(root.0) {
        let path = format!(r"{root_path}\{sub}\Connection");
        if let Some(RegValue::Sz(name)) = backend.reg_get(Hive::Hklm, &path, "Name").ok().flatten() {
            if name.eq_ignore_ascii_case(connection_name) {
                return Some(sub);
            }
        }
    }
    None
}

fn enum_subkeys(key: HKEY) -> Vec<String> {
    let mut out = Vec::new();
    let mut index = 0u32;
    while index < 512 {
        let mut name = vec![0u16; 256];
        let mut len = name.len() as u32;
        let err = unsafe {
            windows::Win32::System::Registry::RegEnumKeyExW(key, index, PWSTR(name.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None)
        };
        if err.0 == 259 {
            break;
        }
        if err == ERROR_MORE_DATA {
            name.resize(1024, 0);
            len = name.len() as u32;
            let err = unsafe {
                windows::Win32::System::Registry::RegEnumKeyExW(key, index, PWSTR(name.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None)
            };
            if err.0 != 0 {
                crate::store::trace(&format!("enumerar subchave {index} falhou (código {})", err.0));
                index += 1;
                continue;
            }
            out.push(String::from_utf16_lossy(&name[..len as usize]));
            index += 1;
            continue;
        }
        if err.0 != 0 {
            crate::store::trace(&format!("enumerar subchave parou no índice {index} (código {})", err.0));
            break;
        }
        out.push(String::from_utf16_lossy(&name[..len as usize]));
        index += 1;
    }
    out
}

fn set_nic_keyword(backend: &mut WindowsBackend, adapter: &str, interface_guid: &str, keyword: &str, value: &str) -> Result<(), BackendError> {
    match put_cim_keyword(backend, adapter, keyword, value) {
        Ok(()) => {
            crate::store::trace(&format!("n-eee CIM gravou {adapter} {keyword}={value}"));
            return Ok(());
        }
        Err(err) => crate::store::trace(&format!("n-eee CIM não gravou {adapter} {keyword}={value}: {err}. caindo para o registro")),
    }
    let sub = find_nic_subkey(backend, interface_guid, adapter).ok_or_else(|| {
        BackendError::Failed(format!("não achei a chave da placa {adapter} (guid {interface_guid})"))
    })?;
    let path = format!(r"{NET_CLASS}\{sub}");
    let existing = backend.reg_get(Hive::Hklm, &path, keyword)?;
    let written = match existing {
        Some(RegValue::Dword(_)) => RegValue::Dword(value.parse().unwrap_or(0)),
        Some(_) => RegValue::Sz(value.to_string()),
        None => return Err(BackendError::NotFound(format!("a placa {adapter} não tem {keyword}"))),
    };
    crate::store::trace(&format!("n-eee registro {path} {keyword}={value}"));
    backend.reg_set(Hive::Hklm, &path, keyword, &written)
}

fn put_cim_keyword(backend: &WindowsBackend, adapter: &str, keyword: &str, value: &str) -> Result<(), BackendError> {
    #[derive(Deserialize)]
    struct Prop {
        Name: Option<String>,
        RegistryKeyword: Option<String>,
        __Path: Option<String>,
    }
    let rows: Vec<Prop> = wmi_query(
        backend,
        r"root\StandardCimv2",
        "caminho da propriedade",
        "SELECT Name, RegistryKeyword, __Path FROM MSFT_NetAdapterAdvancedPropertySettingData",
    )?;
    let path = rows
        .into_iter()
        .find(|row| row.Name.as_deref().unwrap_or("").eq_ignore_ascii_case(adapter) && row.RegistryKeyword.as_deref().unwrap_or("").eq_ignore_ascii_case(keyword))
        .and_then(|row| row.__Path)
        .ok_or_else(|| BackendError::NotFound(format!("CIM sem {keyword} em {adapter}")))?;
    let wmi = backend.wmi(r"root\StandardCimv2")?;
    let variant = string_array_variant(value);
    unsafe {
        let mut object = None;
        wmi.svc
            .GetObject(&windows_core::BSTR::from(path.as_str()), WBEM_GENERIC_FLAG_TYPE(0), None::<&IWbemContext>, Some(&mut object), None)
            .map_err(|err| BackendError::Failed(format!("GetObject {path}: {err}")))?;
        let object = object.ok_or_else(|| BackendError::Failed(format!("GetObject {path} veio vazio")))?;
        object
            .Put(windows::core::w!("RegistryValue"), 0, &variant, 0)
            .map_err(|err| BackendError::Failed(format!("Put RegistryValue: {err}")))?;
        wmi.svc
            .PutInstance(&object, WBEM_GENERIC_FLAG_TYPE(1), None::<&IWbemContext>, None)
            .map_err(|err| BackendError::Failed(format!("PutInstance: {err}")))?;
    }
    Ok(())
}

fn string_array_variant(value: &str) -> windows_core::VARIANT {
    unsafe {
        let array = windows::Win32::System::Ole::SafeArrayCreateVector(windows::Win32::System::Variant::VT_BSTR, 0, 1);
        if array.is_null() {
            return windows_core::VARIANT::from(value);
        }
        let bstr = windows_core::BSTR::from(value);
        let index = 0i32;
        let _ = windows::Win32::System::Ole::SafeArrayPutElement(array, &index, bstr.as_ptr() as *const _);
        windows_core::VARIANT::from_raw(windows_core::imp::VARIANT {
            Anonymous: windows_core::imp::VARIANT_0 {
                Anonymous: windows_core::imp::VARIANT_0_0 {
                    vt: 8 | 8192,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: windows_core::imp::VARIANT_0_0_0 { parray: array as *mut windows_core::imp::SAFEARRAY },
                },
            },
        })
    }
}

fn bounce_adapter(backend: &WindowsBackend, adapter: &str, interface_guid: &str) -> Result<(), BackendError> {
    #[derive(Deserialize)]
    struct AdapterRow {
        __Path: Option<String>,
        Name: Option<String>,
        InterfaceGuid: Option<String>,
    }
    #[derive(Deserialize)]
    struct Ret {
        ReturnValue: Option<u32>,
    }
    #[derive(Deserialize)]
    #[allow(non_camel_case_types)]
    struct MSFT_NetAdapter {}
    let rows = match wmi_query::<AdapterRow>(backend, r"root\StandardCimv2", "religar placa", "SELECT Name, InterfaceGuid, __Path FROM MSFT_NetAdapter") {
        Ok(rows) => rows,
        Err(err) => {
            crate::store::trace(&format!("n-eee reiniciar {adapter} sem consulta: {err}"));
            return Err(err);
        }
    };
    let path = rows
        .iter()
        .find(|row| crate::rules::guids_equal(row.InterfaceGuid.as_deref().unwrap_or(""), interface_guid))
        .or_else(|| rows.iter().find(|row| row.Name.as_deref().unwrap_or("").eq_ignore_ascii_case(adapter)))
        .and_then(|row| row.__Path.clone())
        .ok_or_else(|| BackendError::Failed(format!("não achei {adapter} para reiniciar")))?;
    let wmi = backend.wmi(r"root\StandardCimv2")?;
    wmi.exec_instance_method::<MSFT_NetAdapter, _, Ret>("Disable", &path, ()).map_err(|err| BackendError::Failed(format!("desligar {adapter}: {err}")))?;
    thread::sleep(Duration::from_millis(400));
    wmi.exec_instance_method::<MSFT_NetAdapter, _, Ret>("Enable", &path, ()).map_err(|err| BackendError::Failed(format!("ligar {adapter}: {err}")))?;
    crate::store::trace(&format!("n-eee reiniciei {adapter} guid {interface_guid}"));
    Ok(())
}

fn read_printers() -> Result<Vec<String>, BackendError> {
    let mut needed = 0u32;
    let mut returned = 0u32;
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    unsafe { let _ = EnumPrintersW(flags, PCWSTR::null(), 2, None, &mut needed, &mut returned); }
    if needed == 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; needed as usize];
    unsafe { EnumPrintersW(flags, PCWSTR::null(), 2, Some(&mut buf), &mut needed, &mut returned).map_err(|e| BackendError::Failed(e.to_string()))? };
    let count = returned as usize;
    let infos = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const PRINTER_INFO_2W, count) };
    let mut names = Vec::new();
    for info in infos {
        let name = utf16_z(info.pPrinterName.0);
        let lower = name.to_ascii_lowercase();
        if lower.contains("pdf") || lower.contains("xps") || lower.contains("onenote") || lower.contains("fax") {
            continue;
        }
        if !name.is_empty() {
            names.push(name);
        }
    }
    Ok(names)
}

fn read_startup(backend: &WindowsBackend) -> Result<Vec<StartupEntry>, BackendError> {
    let mut entries = Vec::new();
    push_run(backend, Hive::Hkcu, r"Software\Microsoft\Windows\CurrentVersion\Run", StartupLocation::HkcuRun, &mut entries)?;
    push_run(backend, Hive::Hklm, r"Software\Microsoft\Windows\CurrentVersion\Run", StartupLocation::HklmRun, &mut entries)?;
    push_run(backend, Hive::Hklm, r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run", StartupLocation::HklmRun32, &mut entries)?;
    if let Some(dir) = known_folder(&FOLDERID_Startup)? {
        push_folder(backend, &dir, StartupLocation::UserStartupFolder, &mut entries)?;
    }
    if let Some(dir) = known_folder(&FOLDERID_CommonStartup)? {
        push_folder(backend, &dir, StartupLocation::CommonStartupFolder, &mut entries)?;
    }
    Ok(entries)
}

fn push_run(backend: &WindowsBackend, hive: Hive, path: &str, location: StartupLocation, out: &mut Vec<StartupEntry>) -> Result<(), BackendError> {
    let values = backend.reg_list(hive, path)?;
    let (approved_hive, approved_path) = approved_path(location);
    for (name, value) in values {
        let command = match value {
            RegValue::Sz(s) => s,
            _ => continue,
        };
        let approved = match backend.reg_get(approved_hive, approved_path, &name)? {
            Some(RegValue::Binary(b)) => Some(b),
            _ => None,
        };
        out.push(StartupEntry { name, command, location, approved });
    }
    Ok(())
}

fn push_folder(backend: &WindowsBackend, dir: &str, location: StartupLocation, out: &mut Vec<StartupEntry>) -> Result<(), BackendError> {
    let (hive, path) = approved_path(location);
    let entries = std::fs::read_dir(dir).map_err(|e| BackendError::Failed(format!("pasta de inicialização: {e}")))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.eq_ignore_ascii_case("desktop.ini") {
            continue;
        }
        let command = entry.path().display().to_string();
        let approved = match backend.reg_get(hive, path, &name)? {
            Some(RegValue::Binary(b)) => Some(b),
            _ => None,
        };
        out.push(StartupEntry { name, command, location, approved });
    }
    Ok(())
}

fn approved_path(location: StartupLocation) -> (Hive, &'static str) {
    match location {
        StartupLocation::HkcuRun => (Hive::Hkcu, r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"),
        StartupLocation::HklmRun => (Hive::Hklm, r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"),
        StartupLocation::HklmRun32 => (Hive::Hklm, r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run32"),
        StartupLocation::UserStartupFolder => (Hive::Hkcu, r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder"),
        StartupLocation::CommonStartupFolder => (Hive::Hklm, r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder"),
    }
}

fn known_folder(id: &GUID) -> Result<Option<String>, BackendError> {
    let ptr = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, HANDLE::default()) };
    let ptr = match ptr {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let text = utf16_z(ptr.0);
    unsafe { CoTaskMemFree(Some(ptr.0 as *const _)) };
    if text.is_empty() { Ok(None) } else { Ok(Some(text)) }
}

fn read_security(backend: &WindowsBackend) -> Result<SecurityStatus, BackendError> {
    enable_firmware_privilege();
    let mut secure = None;
    let mut byte = [0u8; 1];
    let n = unsafe {
        GetFirmwareEnvironmentVariableW(w!("SecureBoot"), w!("{8be4df61-93ca-11d2-aa0d-00e098032b8c}"), Some(byte.as_mut_ptr() as *mut _), 1)
    };
    if n == 1 {
        secure = Some(byte[0] != 0);
    } else {
        let err = unsafe { windows::Win32::Foundation::GetLastError() };
        if err == ERROR_ACCESS_DENIED || err == ERROR_PRIVILEGE_NOT_HELD {
            return Err(BackendError::AccessDenied("ler Secure Boot pede administrador".into()));
        }
        secure = Some(false);
    }
    #[derive(Deserialize)]
    struct Tpm { SpecVersion: Option<String>, IsEnabled_InitialValue: Option<bool>, IsActivated_InitialValue: Option<bool> }
    let rows: Vec<Tpm> = wmi_query(backend, r"root\cimv2\Security\MicrosoftTpm", "TPM", "SELECT SpecVersion, IsEnabled_InitialValue, IsActivated_InitialValue FROM Win32_Tpm")?;
    let row = rows.into_iter().next();
    Ok(SecurityStatus {
        secure_boot: secure,
        tpm_present: Some(row.is_some()),
        tpm_ready: row.as_ref().map(|r| r.IsEnabled_InitialValue.unwrap_or(false) && r.IsActivated_InitialValue.unwrap_or(false)),
        spec_version: row.and_then(|r| r.SpecVersion).unwrap_or_default(),
    })
}

fn enable_firmware_privilege() {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token).is_err() {
            return;
        }
        let mut luid = LUID::default();
        if LookupPrivilegeValueW(None, w!("SeSystemEnvironmentPrivilege"), &mut luid).is_err() {
            let _ = CloseHandle(token);
            return;
        }
        let tp = TOKEN_PRIVILEGES { PrivilegeCount: 1, Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }] };
        let _ = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
        let _ = CloseHandle(token);
    }
}

fn restore_frequency(backend: &WindowsBackend) -> Duration {
    match backend.reg_get(Hive::Hklm, r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\SystemRestore", "SystemRestorePointCreationFrequency") {
        Ok(Some(RegValue::Dword(0))) => Duration::from_secs(0),
        Ok(Some(RegValue::Dword(secs))) => Duration::from_secs(secs as u64),
        _ => Duration::from_secs(24 * 3600),
    }
}

fn list_restore_points(backend: &WindowsBackend) -> Result<Vec<RestorePointInfo>, BackendError> {
    #[derive(Deserialize)]
    struct Row { SequenceNumber: Option<u32>, Description: Option<String>, CreationTime: Option<String> }
    let rows: Vec<Row> = wmi_query(backend, r"root\default", "pontos de restauração", "SELECT SequenceNumber, Description, CreationTime FROM SystemRestore")?;
    let mut out = Vec::new();
    for row in rows {
        let when = row.CreationTime.as_deref().and_then(parse_wmi_time);
        out.push(RestorePointInfo {
            sequence: row.SequenceNumber.unwrap_or(0) as i64,
            description: row.Description.unwrap_or_else(|| "Ponto de restauração".into()),
            created_rfc3339: when.map(|t| t.to_rfc3339()).unwrap_or_default(),
        });
    }
    Ok(out)
}

fn recent_restore(backend: &WindowsBackend) -> Result<Option<RestorePointInfo>, BackendError> {
    let window = restore_frequency(backend);
    if window.is_zero() {
        crate::store::trace("ponto de restauração: SystemRestorePointCreationFrequency=0, sem reaproveitar ponto antigo");
        return Ok(None);
    }
    let mut best: Option<(RestorePointInfo, chrono::DateTime<chrono::Utc>)> = None;
    for info in list_restore_points(backend)? {
        let Some(when) = chrono::DateTime::parse_from_rfc3339(&info.created_rfc3339).ok().map(|t| t.with_timezone(&chrono::Utc)) else { continue };
        let age = chrono::Utc::now().signed_duration_since(when);
        if age.num_seconds() > window.as_secs() as i64 || age.num_seconds() < 0 {
            continue;
        }
        if best.as_ref().map(|(_, t)| when > *t).unwrap_or(true) {
            best = Some((info, when));
        }
    }
    Ok(best.map(|(info, _)| info))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn com_mta_recusado_na_thread_da_janela() {
        mark_ui_thread();
        match WindowsBackend::new() {
            Err(err) => assert!(err.contains("janela"), "{err}"),
            Ok(_) => panic!("a thread da janela não pode inicializar COM"),
        }
    }
}

fn parse_wmi_time(text: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if text.len() < 14 {
        return None;
    }
    let year: i32 = text[0..4].parse().ok()?;
    let month: u32 = text[4..6].parse().ok()?;
    let day: u32 = text[6..8].parse().ok()?;
    let hour: u32 = text[8..10].parse().ok()?;
    let min: u32 = text[10..12].parse().ok()?;
    let sec: u32 = text[12..14].parse().ok()?;
    chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, min, sec).map(|n| n.and_utc())
}

fn set_system_protection(backend: &mut WindowsBackend, enabled: bool) -> Result<(), BackendError> {
    #[derive(Deserialize)]
    #[allow(non_camel_case_types)]
    struct SystemRestore {}
    #[derive(serde::Serialize)]
    struct DriveArg {
        Drive: String,
    }
    #[derive(Deserialize)]
    struct Ret {
        ReturnValue: Option<u32>,
    }
    let drive = windows_drive();
    let method = if enabled { "Enable" } else { "Disable" };
    crate::store::trace(&format!("ação proteção do sistema {method} {drive}"));
    let wmi = backend.wmi(r"root\default")?;
    let out: Ret = wmi.exec_class_method::<SystemRestore, _, _>(method, DriveArg { Drive: drive.clone() }).map_err(wmi_err)?;
    let code = out.ReturnValue.unwrap_or(0);
    crate::store::trace(&format!("ação proteção {method} ReturnValue={code}"));
    if code != 0 {
        let verb = if enabled { "ligar" } else { "desligar" };
        return Err(BackendError::Failed(format!("não consegui {verb} a Proteção do Sistema em {drive} (código {code})")));
    }
    let flag = if enabled { 0 } else { 1 };
    let _ = backend.reg_set(Hive::Hklm, r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\SystemRestore", "DisableSR", &RegValue::Dword(flag));
    Ok(())
}

fn windows_drive() -> String {
    let mut buf = [0u16; 260];
    let n = unsafe { GetWindowsDirectoryW(Some(&mut buf)) };
    if n == 0 {
        return r"C:\".into();
    }
    let text = String::from_utf16_lossy(&buf[..n as usize]);
    let letter = text.chars().next().unwrap_or('C');
    format!(r"{letter}:\")
}

fn create_restore(description: &str) -> Result<RestorePointInfo, BackendError> {
    let mut info = RESTOREPOINTINFOW::default();
    info.dwEventType = BEGIN_SYSTEM_CHANGE;
    info.dwRestorePtType = MODIFY_SETTINGS;
    let words = wide(description);
    let n = words.len().min(255);
    unsafe {
        let dest = std::ptr::addr_of_mut!(info.szDescription);
        std::ptr::copy_nonoverlapping(words.as_ptr(), dest.cast::<u16>(), n);
    }
    let mut status = STATEMGRSTATUS::default();
    let ok = unsafe { SRSetRestorePointW(&info, &mut status) };
    if !ok.as_bool() {
        let code = unsafe { std::ptr::addr_of!(status.nStatus).read_unaligned().0 };
        crate::store::trace(&format!("ação criar ponto recusado código={code}"));
        if code == 1058 {
            return Err(BackendError::Failed("PROTECTION_DISABLED".into()));
        }
        if code == 5 {
            return Err(BackendError::AccessDenied("criar o ponto de restauração precisa de administrador".into()));
        }
        return Err(BackendError::Failed(format!("o Windows recusou o ponto de restauração (código {code})")));
    }
    crate::store::trace("ação criar ponto aceito pelo Windows");
    let sequence = unsafe { std::ptr::addr_of!(status.llSequenceNumber).read_unaligned() };
    info.dwEventType = END_SYSTEM_CHANGE;
    unsafe { std::ptr::addr_of_mut!(info.llSequenceNumber).write_unaligned(sequence); }
    let mut end_status = STATEMGRSTATUS::default();
    let _ = unsafe { SRSetRestorePointW(&info, &mut end_status) };
    Ok(RestorePointInfo { sequence, description: description.to_string(), created_rfc3339: now_rfc3339() })
}
