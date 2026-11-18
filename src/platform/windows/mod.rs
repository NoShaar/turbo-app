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
    CloseHandle, LocalFree, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PRIVILEGE_NOT_HELD, HANDLE, HLOCAL, LUID, WIN32_ERROR,
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
use windows::Win32::System::Power::{PowerGetActiveScheme, PowerSetActiveScheme};
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

pub struct WindowsBackend {
    _com: COMLibrary,
}

impl WindowsBackend {
    pub fn new() -> Result<Self, String> {
        let com = COMLibrary::new().map_err(|e| e.to_string())?;
        Ok(Self { _com: com })
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

    fn set_adapter_keyword(&mut self, adapter: &str, keyword: &str, value: &str) -> Result<(), BackendError> {
        set_nic_keyword(self, adapter, keyword, value)
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

    fn recent_restore_point(&self) -> Result<Option<RestorePointInfo>, BackendError> {
        recent_restore(self)
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
    let msg = err.to_string();
    let lower = msg.to_ascii_lowercase();
    if lower.contains("access") || lower.contains("privilege") || msg.contains("80041003") || msg.contains("80070005") {
        BackendError::AccessDenied(msg)
    } else {
        BackendError::Failed(msg)
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
            if msg.to_ascii_lowercase().contains("access") {
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
    let wmi = backend.wmi(r"root\Microsoft\Windows\Storage")?;
    let disks: Vec<DiskRow> = wmi.raw_query("SELECT FriendlyName, MediaType, BusType, HealthStatus, DeviceId FROM MSFT_PhysicalDisk").map_err(wmi_err)?;
    let counters: Vec<CounterRow> = wmi.raw_query("SELECT DeviceId, Temperature, TemperatureMax, Wear, PowerOnHours, ReadErrorsUncorrected, WriteErrorsUncorrected FROM MSFT_StorageReliabilityCounter").unwrap_or_default();
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
    let wmi = backend.wmi("root\\cimv2")?;
    #[derive(Deserialize)]
    struct Link { Antecedent: Option<String>, Dependent: Option<String> }
    #[derive(Deserialize)]
    struct Page { Name: Option<String>, AllocatedBaseSize: Option<u64> }
    let links: Vec<Link> = wmi.raw_query("SELECT Antecedent, Dependent FROM Win32_LogicalDiskToPartition").map_err(wmi_err)?;
    let mut letter_disk: BTreeMap<String, u32> = BTreeMap::new();
    for link in links {
        let Some(dep) = link.Dependent else { continue };
        let Some(ant) = link.Antecedent else { continue };
        if let (Some(letter), Some(index)) = (drive_letter(&dep), disk_index(&ant)) {
            letter_disk.insert(letter.to_ascii_uppercase(), index);
        }
    }
    let usage: Vec<Page> = wmi.raw_query("SELECT Name, AllocatedBaseSize FROM Win32_PageFileUsage").unwrap_or_default();
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
    #[derive(Deserialize)]
    struct Row {
        Name: Option<String>,
        NdisPhysicalMedium: Option<u32>,
        LinkSpeed: Option<u64>,
        HardwareInterface: Option<bool>,
        MediaConnectState: Option<u16>,
        InterfaceDescription: Option<String>,
    }
    let wmi = backend.wmi(r"root\StandardCimv2")?;
    let rows: Vec<Row> = wmi.raw_query("SELECT Name, NdisPhysicalMedium, LinkSpeed, HardwareInterface, MediaConnectState, InterfaceDescription FROM MSFT_NetAdapter").map_err(wmi_err)?;
    let mut out = Vec::new();
    for row in rows {
        if row.HardwareInterface == Some(false) {
            continue;
        }
        let name = row.Name.clone().unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let medium = row.NdisPhysicalMedium.unwrap_or(0);
        let desc = row.InterfaceDescription.unwrap_or_default().to_ascii_lowercase();
        let wired = medium == 14 || (!desc.contains("wi-fi") && !desc.contains("wireless") && !desc.contains("wlan") && medium != 9);
        let connected = row.MediaConnectState == Some(1);
        let props = nic_props(backend, &name);
        let max_bps = props.iter().find_map(|p| p.display_name.contains("Speed").then_some(0)).unwrap_or(0);
        let link = row.LinkSpeed.unwrap_or(0);
        let max_bps = if max_bps == 0 { link.max(guess_max(link, wired)) } else { max_bps };
        out.push(AdapterInfo { name, wired, connected, link_bps: link, max_bps, power_props: props });
    }
    Ok(out)
}

fn guess_max(link: u64, wired: bool) -> u64 {
    if !wired {
        return link;
    }
    if link > 0 && link < 1_000_000_000 { 1_000_000_000 } else { link }
}

fn nic_props(backend: &WindowsBackend, adapter: &str) -> Vec<NicPowerProp> {
    let Ok(wmi) = backend.wmi(r"root\StandardCimv2") else { return registry_nic_props(backend, adapter); };
    #[derive(Deserialize, Default)]
    struct Prop {
        Name: Option<String>,
        DisplayName: Option<String>,
        RegistryKeyword: Option<String>,
        DisplayValue: Option<String>,
        RegistryValue: Option<Vec<String>>,
        ValidDisplayValues: Option<Vec<String>>,
        ValidRegistryValues: Option<Vec<String>>,
    }
    let query = format!("SELECT Name, DisplayName, RegistryKeyword, DisplayValue, RegistryValue, ValidDisplayValues, ValidRegistryValues FROM MSFT_NetAdapterAdvancedPropertySettingData WHERE Name = '{adapter}'");
    let rows: Vec<Prop> = match wmi.raw_query(&query) {
        Ok(rows) => rows,
        Err(_) => return registry_nic_props(backend, adapter),
    };
    rows.into_iter().filter_map(|row| {
        let display = row.DisplayName.unwrap_or_default();
        if !is_power_prop(&display) {
            return None;
        }
        let current = row.RegistryValue.and_then(|v| v.into_iter().next()).unwrap_or_default();
        let (disabled_value, disabled_display) = disabled_pair(row.ValidDisplayValues.as_deref(), row.ValidRegistryValues.as_deref());
        Some(NicPowerProp {
            display_name: display,
            keyword: row.RegistryKeyword.unwrap_or_default(),
            current,
            current_display: row.DisplayValue.unwrap_or_default(),
            disabled_value,
            disabled_display,
        })
    }).collect()
}

fn is_power_prop(name: &str) -> bool {
    const NAMES: &[&str] = &["energia eficiente ethernet", "energy-efficient ethernet", "energy efficient ethernet", "power saving mode", "green ethernet"];
    NAMES.iter().any(|n| name.eq_ignore_ascii_case(n))
}

fn disabled_pair(displays: Option<&[String]>, values: Option<&[String]>) -> (String, String) {
    let displays = displays.unwrap_or(&[]);
    let values = values.unwrap_or(&[]);
    for (display, value) in displays.iter().zip(values.iter()) {
        let d = display.to_ascii_lowercase();
        if d.contains("desativ") || d.contains("disabled") || d == "off" {
            return (value.clone(), display.clone());
        }
    }
    ("0".into(), "Desativado".into())
}

fn registry_nic_props(backend: &WindowsBackend, adapter: &str) -> Vec<NicPowerProp> {
    let Some(sub) = find_nic_subkey(backend, adapter) else { return Vec::new(); };
    let path = format!(r"{NET_CLASS}\{sub}");
    let Ok(values) = backend.reg_list(Hive::Hklm, &path) else { return Vec::new(); };
    let mut out = Vec::new();
    for (name, value) in values {
        let display = keyword_display(&name);
        if display.is_none() {
            continue;
        }
        let current = match value {
            RegValue::Sz(s) => s,
            RegValue::Dword(n) => n.to_string(),
            _ => continue,
        };
        let off = current == "0";
        out.push(NicPowerProp {
            display_name: display.unwrap(),
            keyword: name,
            current_display: if off { "Desativado".into() } else { "Ativado".into() },
            disabled_value: "0".into(),
            disabled_display: "Desativado".into(),
            current,
        });
    }
    out
}

fn keyword_display(keyword: &str) -> Option<String> {
    let k = keyword.to_ascii_lowercase();
    if k.contains("eee") {
        Some("Energy-Efficient Ethernet".into())
    } else if k.contains("powersaving") {
        Some("Power Saving Mode".into())
    } else if k.contains("greenethernet") || k.contains("green ethernet") {
        Some("Green Ethernet".into())
    } else {
        None
    }
}

fn find_nic_subkey(backend: &WindowsBackend, adapter: &str) -> Option<String> {
    let root = open_key(Hive::Hklm, NET_CLASS, KEY_READ.0).ok()??;
    for index in 0..64 {
        let mut name = [0u16; 32];
        let mut len = name.len() as u32;
        let err = unsafe { windows::Win32::System::Registry::RegEnumKeyExW(root.0, index, PWSTR(name.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None) };
        if err.0 != 0 {
            break;
        }
        let sub = String::from_utf16_lossy(&name[..len as usize]);
        let path = format!(r"{NET_CLASS}\{sub}");
        let driver = backend.reg_get(Hive::Hklm, &path, "DriverDesc").ok().flatten();
        let netcfg = backend.reg_get(Hive::Hklm, &path, "NetCfgInstanceId").ok().flatten();
        let matches = match (&driver, &netcfg) {
            (Some(RegValue::Sz(text)), _) if text.eq_ignore_ascii_case(adapter) => true,
            (_, Some(RegValue::Sz(text))) if text.eq_ignore_ascii_case(adapter) => true,
            (Some(RegValue::Sz(text)), _) if text.to_ascii_lowercase().contains(&adapter.to_ascii_lowercase()) => true,
            _ => false,
        };
        if matches {
            return Some(sub);
        }
    }
    None
}

fn set_nic_keyword(backend: &mut WindowsBackend, adapter: &str, keyword: &str, value: &str) -> Result<(), BackendError> {
    let sub = find_nic_subkey(backend, adapter).ok_or_else(|| BackendError::Failed(format!("não achei a chave da placa {adapter}")))?;
    let path = format!(r"{NET_CLASS}\{sub}");
    let existing = backend.reg_get(Hive::Hklm, &path, keyword)?;
    let written = match existing {
        Some(RegValue::Dword(_)) => RegValue::Dword(value.parse().unwrap_or(0)),
        _ => RegValue::Sz(value.to_string()),
    };
    backend.reg_set(Hive::Hklm, &path, keyword, &written)?;
    bounce_adapter(backend, adapter);
    Ok(())
}

fn bounce_adapter(backend: &WindowsBackend, adapter: &str) {
    #[derive(Deserialize)]
    struct AdapterRow { __Path: Option<String>, Name: Option<String> }
    #[derive(Deserialize)]
    struct Ret { ReturnValue: Option<u32> }
#[derive(Deserialize)]
#[allow(non_camel_case_types)]
struct MSFT_NetAdapter {}
    let Ok(wmi) = backend.wmi(r"root\StandardCimv2") else { return };
    let Ok(rows): Result<Vec<AdapterRow>, _> = wmi.raw_query("SELECT Name, __Path FROM MSFT_NetAdapter").map_err(wmi_err) else { return };
    let Some(path) = rows.into_iter().find(|r| r.Name.as_deref().unwrap_or("").eq_ignore_ascii_case(adapter)).and_then(|r| r.__Path) else { return };
    let _ = wmi.exec_instance_method::<MSFT_NetAdapter, _, Ret>("Disable", &path, ());
    thread::sleep(Duration::from_millis(400));
    let _ = wmi.exec_instance_method::<MSFT_NetAdapter, _, Ret>("Enable", &path, ());
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
    let wmi = backend.wmi(r"root\cimv2\Security\MicrosoftTpm")?;
    let rows: Vec<Tpm> = wmi.raw_query("SELECT SpecVersion, IsEnabled_InitialValue, IsActivated_InitialValue FROM Win32_Tpm").map_err(wmi_err)?;
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

fn recent_restore(backend: &WindowsBackend) -> Result<Option<RestorePointInfo>, BackendError> {
    #[derive(Deserialize)]
    struct Row { SequenceNumber: Option<u32>, Description: Option<String>, CreationTime: Option<String> }
    let wmi = match backend.wmi(r"root\default") {
        Ok(w) => w,
        Err(BackendError::AccessDenied(_)) => return Ok(None),
        Err(err) => return Err(err),
    };
    let rows: Vec<Row> = match wmi.raw_query("SELECT SequenceNumber, Description, CreationTime FROM SystemRestore") {
        Ok(rows) => rows,
        Err(_) => return Ok(None),
    };
    let mut best: Option<RestorePointInfo> = None;
    for row in rows {
        let Some(when) = row.CreationTime.as_deref().and_then(parse_wmi_time) else { continue };
        let age = chrono::Utc::now().signed_duration_since(when);
        if age.num_hours() > 24 || age.num_hours() < 0 {
            continue;
        }
        let info = RestorePointInfo {
            sequence: row.SequenceNumber.unwrap_or(0) as i64,
            description: row.Description.unwrap_or_else(|| "Ponto de restauração".into()),
            created_rfc3339: when.to_rfc3339(),
        };
        if best.as_ref().map(|b| info.sequence > b.sequence).unwrap_or(true) {
            best = Some(info);
        }
    }
    Ok(best)
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
        if code == 1058 {
            return Err(BackendError::Failed("PROTECTION_DISABLED".into()));
        }
        return Err(BackendError::Failed(format!("ponto de restauração recusado (código {code})")));
    }
    let sequence = unsafe { std::ptr::addr_of!(status.llSequenceNumber).read_unaligned() };
    info.dwEventType = END_SYSTEM_CHANGE;
    unsafe { std::ptr::addr_of_mut!(info.llSequenceNumber).write_unaligned(sequence); }
    let mut end_status = STATEMGRSTATUS::default();
    let _ = unsafe { SRSetRestorePointW(&info, &mut end_status) };
    Ok(RestorePointInfo { sequence, description: description.to_string(), created_rfc3339: now_rfc3339() })
}
