//! Interface do sistema. Os testes usam um backend falso; no Windows, a
//! implementação em `platform::windows` fala com registro, serviços e WMI.

use crate::model::*;

pub trait SystemBackend {
    fn reg_get(&self, hive: Hive, path: &str, name: &str) -> Result<Option<RegValue>, BackendError>;
    fn reg_set(
        &mut self,
        hive: Hive,
        path: &str,
        name: &str,
        value: &RegValue,
    ) -> Result<(), BackendError>;
    fn reg_delete(&mut self, hive: Hive, path: &str, name: &str) -> Result<(), BackendError>;
    fn reg_list(&self, hive: Hive, path: &str) -> Result<Vec<(String, RegValue)>, BackendError>;

    fn service(&self, name: &str) -> Result<ServiceInfo, BackendError>;
    fn set_service_start(&mut self, name: &str, start: StartType) -> Result<(), BackendError>;
    fn start_service(&mut self, name: &str) -> Result<(), BackendError>;
    fn stop_service(&mut self, name: &str) -> Result<(), BackendError>;

    fn power_active(&self) -> Result<String, BackendError>;
    fn power_set_active(&mut self, guid: &str) -> Result<(), BackendError>;

    fn mouse_accel(&self) -> Result<MouseAccel, BackendError>;
    fn set_mouse_accel(&mut self, value: MouseAccel) -> Result<(), BackendError>;
    fn mouse_speed(&self) -> Result<u32, BackendError>;
    fn set_mouse_speed(&mut self, speed: u32) -> Result<(), BackendError>;

    fn sticky_flags(&self) -> Result<u32, BackendError>;
    fn set_sticky_flags(&mut self, flags: u32) -> Result<(), BackendError>;
    fn filter_flags(&self) -> Result<u32, BackendError>;
    fn set_filter_flags(&mut self, flags: u32) -> Result<(), BackendError>;
    fn toggle_flags(&self) -> Result<u32, BackendError>;
    fn set_toggle_flags(&mut self, flags: u32) -> Result<(), BackendError>;

    fn display(&self) -> Result<DisplayInfo, BackendError>;
    fn set_refresh(&mut self, device: &str, hz: u32) -> Result<(), BackendError>;

    fn disks(&self) -> Result<Vec<DiskHealth>, BackendError>;
    fn disk_events(&self) -> Result<Vec<DiskEventGroup>, BackendError>;
    fn sample_disk_usage(&mut self) -> Result<DiskReport, BackendError>;
    fn windows_location(&self) -> Result<WindowsLocation, BackendError>;

    fn adapters(&self) -> Result<Vec<AdapterInfo>, BackendError>;
    fn set_adapter_keyword(
        &mut self,
        adapter: &str,
        keyword: &str,
        value: &str,
    ) -> Result<(), BackendError>;

    fn physical_printers(&self) -> Result<Vec<String>, BackendError>;
    fn startup_entries(&self) -> Result<Vec<StartupEntry>, BackendError>;
    fn security_status(&self) -> Result<SecurityStatus, BackendError>;

    fn protection_enabled(&self) -> Result<bool, BackendError>;
    fn recent_restore_point(&self) -> Result<Option<RestorePointInfo>, BackendError>;
    fn create_restore_point(&mut self, description: &str) -> Result<RestorePointInfo, BackendError>;

    fn is_elevated(&self) -> bool;
    fn open_resource_monitor(&mut self) -> Result<(), BackendError>;
}
