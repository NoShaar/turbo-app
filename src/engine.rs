//! Orquestra diagnóstico, backup obrigatório, aplicação e reversão.

use crate::backend::SystemBackend;
use crate::catalog::{self, meta};
use crate::model::*;
use crate::store::{self, LogEntry};
use crate::tweaks::{self, ReadCache};
use std::path::Path;

pub const PROTECTION_OFF_MESSAGE: &str = "\
A Proteção do Sistema está desligada, então o Windows não cria ponto de restauração.\n\
O Turbo ainda pode guardar, num JSON, o valor original de cada ajuste.\n\
Para ter o ponto também: Painel de Controle > Sistema > Proteção do Sistema > Configurar > Ativar proteção.\n\
Sem o ponto, a reversão do Turbo continua valendo para o que ele mesmo alterou.";

pub const RESTORE_24H_NOTE: &str = "\
O Windows só cria 1 ponto de restauração a cada 24 horas. Já existe um de hoje; o Turbo usa esse e guarda os valores originais no JSON.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub id: String,
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizePlan {
    pub local: Vec<String>,
    pub needs_admin: Vec<String>,
}

pub fn create_backup(sys: &mut dyn SystemBackend, ack_no_protection: bool) -> Result<BackupFile, EngineError> {
    let restore_point = decide_restore_point(sys, ack_no_protection)?;
    let mut items = Vec::new();
    for item in catalog::ITEMS.iter().filter(|i| !i.readonly) {
        let snapshot = tweaks::capture(item.id, sys)?;
        items.push(BackupItem { id: item.id.to_string(), snapshot });
    }
    Ok(BackupFile {
        version: 1,
        app_version: APP_VERSION.to_string(),
        created_at: now_rfc3339(),
        restore_point,
        items,
    })
}

fn decide_restore_point(sys: &mut dyn SystemBackend, ack_no_protection: bool) -> Result<RestorePointRecord, EngineError> {
    let enabled = sys.protection_enabled()?;
    if !enabled {
        if !ack_no_protection {
            return Err(EngineError::ProtectionDisabled { message: PROTECTION_OFF_MESSAGE.to_string() });
        }
        return Ok(RestorePointRecord {
            status: RestorePointStatus::ProtectionDisabled,
            message: PROTECTION_OFF_MESSAGE.to_string(),
            sequence: None,
            acknowledged_no_protection: true,
        });
    }
    if let Some(existing) = sys.recent_restore_point()? {
        return Ok(RestorePointRecord {
            status: RestorePointStatus::UsedExisting,
            message: format!("{RESTORE_24H_NOTE} Ponto: {} ({}).", existing.description, existing.sequence),
            sequence: Some(existing.sequence),
            acknowledged_no_protection: false,
        });
    }
    match sys.create_restore_point("Turbo App") {
        Ok(info) => Ok(RestorePointRecord {
            status: RestorePointStatus::Created,
            message: format!("Ponto de restauração criado: {} ({}).", info.description, info.sequence),
            sequence: Some(info.sequence),
            acknowledged_no_protection: false,
        }),
        Err(BackendError::Failed(msg)) if msg.contains("PROTECTION_DISABLED") => {
            if !ack_no_protection {
                return Err(EngineError::ProtectionDisabled { message: PROTECTION_OFF_MESSAGE.to_string() });
            }
            Ok(RestorePointRecord {
                status: RestorePointStatus::ProtectionDisabled,
                message: PROTECTION_OFF_MESSAGE.to_string(),
                sequence: None,
                acknowledged_no_protection: true,
            })
        }
        Err(BackendError::Failed(msg)) if msg.contains("RESTORE_POINT_24H") => {
            let existing = sys.recent_restore_point()?;
            Ok(RestorePointRecord {
                status: RestorePointStatus::UsedExisting,
                message: RESTORE_24H_NOTE.to_string(),
                sequence: existing.map(|e| e.sequence),
                acknowledged_no_protection: false,
            })
        }
        Err(err) => Ok(RestorePointRecord {
            status: RestorePointStatus::Failed,
            message: format!("Não foi possível criar o ponto de restauração ({err}). O JSON com os valores originais foi salvo mesmo assim."),
            sequence: None,
            acknowledged_no_protection: false,
        }),
    }
}

pub fn diagnose_all(sys: &dyn SystemBackend, ctx: &ReadCache<'_>, backup: Option<&BackupFile>) -> Result<Vec<ItemView>, EngineError> {
    let mut views = Vec::with_capacity(catalog::ITEMS.len());
    for item in catalog::ITEMS {
        views.push(view_one(sys, ctx, backup, item.id)?);
    }
    Ok(views)
}

pub fn view_one(sys: &dyn SystemBackend, ctx: &ReadCache<'_>, backup: Option<&BackupFile>, id: &str) -> Result<ItemView, EngineError> {
    let item = meta(id).ok_or_else(|| EngineError::Message(format!("item desconhecido: {id}")))?;
    let diagnosis = tweaks::diagnose(id, sys, ctx)?;
    let can_revert = can_revert(sys, backup, id);
    Ok(ItemView {
        id: id.to_string(),
        section: item.section,
        name: item.name.to_string(),
        faz: item.faz.to_string(),
        ajuda: item.ajuda.to_string(),
        ok_when: item.ok_when.to_string(),
        admin: item.admin.to_string(),
        reinicio: item.reinicio.to_string(),
        seal: item.seal,
        caveat: item.caveat.unwrap_or("").to_string(),
        readonly: item.readonly,
        conditional: item.conditional,
        state: diagnosis.state,
        detail: diagnosis.detail,
        can_optimize: !item.readonly && diagnosis.offer_apply && diagnosis.state == ItemState::CanImprove,
        can_revert,
        needs_admin_read: diagnosis.state == ItemState::NeedsAdmin,
    })
}

fn can_revert(sys: &dyn SystemBackend, backup: Option<&BackupFile>, id: &str) -> bool {
    let Some(backup) = backup else {
        return false;
    };
    let Some(snap) = backup.snapshot(id) else {
        return false;
    };
    match tweaks::capture(id, sys) {
        Ok(current) => &current != snap,
        Err(_) => true,
    }
}

pub fn plan_optimize(sys: &dyn SystemBackend, ctx: &ReadCache<'_>, section: Option<Section>) -> Result<OptimizePlan, EngineError> {
    let views = diagnose_all(sys, ctx, None)?;
    let mut local = Vec::new();
    let mut needs_admin = Vec::new();
    for view in views {
        if let Some(section) = section {
            if view.section != section {
                continue;
            }
        }
        // O botão geral da seção (e o da tela inicial) ignora leitura e condicionais.
        if view.readonly || view.conditional || !view.can_optimize {
            continue;
        }
        if tweaks::requires_admin(&view.id, sys)? && !sys.is_elevated() {
            needs_admin.push(view.id);
        } else {
            local.push(view.id);
        }
    }
    Ok(OptimizePlan { local, needs_admin })
}

pub fn plan_revert(sys: &dyn SystemBackend, ctx: &ReadCache<'_>, backup: Option<&BackupFile>, section: Option<Section>) -> Result<OptimizePlan, EngineError> {
    let views = diagnose_all(sys, ctx, backup)?;
    let mut local = Vec::new();
    let mut needs_admin = Vec::new();
    for view in views {
        if let Some(section) = section {
            if view.section != section {
                continue;
            }
        }
        if !view.can_revert {
            continue;
        }
        if tweaks::requires_admin(&view.id, sys)? && !sys.is_elevated() {
            needs_admin.push(view.id);
        } else {
            local.push(view.id);
        }
    }
    Ok(OptimizePlan { local, needs_admin })
}

pub fn apply_one(
    sys: &mut dyn SystemBackend,
    ctx: &ReadCache<'_>,
    backup: &BackupFile,
    id: &str,
    log_path: Option<&Path>,
) -> Result<ApplyOutcome, EngineError> {
    let item = meta(id).ok_or_else(|| EngineError::Message(format!("item desconhecido: {id}")))?;
    if item.readonly {
        return Err(EngineError::ReadOnly);
    }
    if backup.snapshot(id).is_none() {
        return Err(EngineError::BackupRequired);
    }
    if tweaks::requires_admin(id, sys)? && !sys.is_elevated() {
        return Err(EngineError::NeedsAdmin);
    }
    let before = tweaks::capture(id, sys)?;
    let message = match tweaks::apply(id, sys, ctx) {
        Ok(message) => message,
        Err(err) => {
            let _ = write_log(log_path, id, "apply", false, &err.to_string(), &before, &before);
            return Err(err);
        }
    };
    let after_view = tweaks::diagnose(id, sys, ctx)?;
    if after_view.state != ItemState::Ok {
        if let Some(snap) = backup.snapshot(id) {
            let _ = tweaks::revert(id, sys, snap);
        }
        let err = EngineError::Message(format!(
            "Depois de aplicar, a leitura não confirmou o esperado ({}). O Turbo restaurou o valor original.",
            after_view.detail.lines().next().unwrap_or("")
        ));
        let _ = write_log(log_path, id, "apply", false, &err.to_string(), &before, &before);
        return Err(err);
    }
    let after = tweaks::capture(id, sys).unwrap_or(before.clone());
    let _ = write_log(log_path, id, "apply", true, &message, &before, &after);
    Ok(ApplyOutcome { id: id.to_string(), ok: true, message })
}

pub fn revert_one(
    sys: &mut dyn SystemBackend,
    backup: &BackupFile,
    id: &str,
    log_path: Option<&Path>,
) -> Result<ApplyOutcome, EngineError> {
    let snap = backup.snapshot(id).ok_or(EngineError::BackupRequired)?;
    if tweaks::requires_admin(id, sys)? && !sys.is_elevated() {
        return Err(EngineError::NeedsAdmin);
    }
    let before = tweaks::capture(id, sys).ok();
    let message = tweaks::revert(id, sys, snap)?;
    let after = tweaks::capture(id, sys)?;
    if &after != snap {
        let err = EngineError::Message("A reversão não confirmou o valor original. Nada mais foi alterado por este item.".into());
        let _ = write_log(log_path, id, "revert", false, &err.to_string(), &before, &after);
        return Err(err);
    }
    let _ = write_log(log_path, id, "revert", true, &message, &before, &after);
    Ok(ApplyOutcome { id: id.to_string(), ok: true, message })
}

pub fn apply_ids(
    sys: &mut dyn SystemBackend,
    ctx: &ReadCache<'_>,
    backup: &BackupFile,
    ids: &[String],
    log_path: Option<&Path>,
) -> Vec<Result<ApplyOutcome, String>> {
    ids.iter()
        .map(|id| apply_one(sys, ctx, backup, id, log_path).map_err(|e| format!("{id}: {e}")))
        .collect()
}

pub fn revert_ids(
    sys: &mut dyn SystemBackend,
    backup: &BackupFile,
    ids: &[String],
    log_path: Option<&Path>,
) -> Vec<Result<ApplyOutcome, String>> {
    ids.iter()
        .map(|id| revert_one(sys, backup, id, log_path).map_err(|e| format!("{id}: {e}")))
        .collect()
}

fn write_log(path: Option<&Path>, id: &str, action: &str, ok: bool, message: &str, before: &impl serde::Serialize, after: &impl serde::Serialize) -> Result<(), String> {
    let Some(path) = path else {
        return Ok(());
    };
    store::log_change(path, id, action, ok, message, before, after)
}

pub fn backup_log_entry(backup: &BackupFile) -> LogEntry {
    LogEntry {
        ts: now_rfc3339(),
        id: "backup".into(),
        action: "backup".into(),
        ok: true,
        message: backup.restore_point.message.clone(),
        before: serde_json::Value::Null,
        after: serde_json::json!({
            "status": backup.restore_point.status,
            "items": backup.items.len(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeBackend;
    use crate::model::Hive;
    use std::time::{Duration, SystemTime};

    fn ready() -> FakeBackend {
        let mut sys = FakeBackend::carlos_demo();
        sys.elevated = true;
        sys.deny_admin_reads = false;
        sys.services.insert(
            "SysMain".into(),
            ServiceInfo { name: "SysMain".into(), run: ServiceRun::Running, start: StartType::Auto },
        );
        sys.services.insert(
            "Spooler".into(),
            ServiceInfo { name: "Spooler".into(), run: ServiceRun::Running, start: StartType::Auto },
        );
        sys
    }

    fn backup_of(sys: &mut FakeBackend) -> BackupFile {
        create_backup(sys, false).unwrap()
    }

    #[test]
    fn apply_without_backup_is_refused() {
        let mut sys = ready();
        let err = apply_one(&mut sys, &ReadCache::empty(), &BackupFile {
            version: 1,
            app_version: "1.0.0".into(),
            created_at: String::new(),
            restore_point: RestorePointRecord {
                status: RestorePointStatus::Created,
                message: String::new(),
                sequence: None,
                acknowledged_no_protection: false,
            },
            items: vec![],
        }, "p-dvr", None).unwrap_err();
        assert!(matches!(err, EngineError::BackupRequired));
        assert!(sys.reg_get(Hive::Hkcu, r"System\GameConfigStore", "GameDVR_Enabled").unwrap().is_some());
    }

    #[test]
    fn backup_remembers_missing_values_and_revert_deletes_them() {
        let mut sys = ready();
        // Modo de Jogo ausente é OK e não seria aplicado. Forçamos um valor 0
        // para o apply criar a chave e o revert apagar.
        sys.put(Hive::Hkcu, r"Software\Microsoft\GameBar", "AutoGameModeEnabled", RegValue::Dword(0));
        let backup = backup_of(&mut sys);
        let snap = backup.snapshot("p-gamemode").unwrap();
        match snap {
            ItemSnapshot::Registry { entries } => {
                assert_eq!(entries[0].value, Some(RegValue::Dword(0)));
            }
            other => panic!("snapshot inesperado: {other:?}"),
        }
        apply_one(&mut sys, &ReadCache::empty(), &backup, "p-gamemode", None).unwrap();
        assert_eq!(
            sys.reg_get(Hive::Hkcu, r"Software\Microsoft\GameBar", "AutoGameModeEnabled").unwrap(),
            Some(RegValue::Dword(1))
        );
        // Troca o snapshot do teste por "não existia" para cobrir o apagamento.
        let mut backup = backup;
        if let Some(item) = backup.items.iter_mut().find(|i| i.id == "p-gamemode") {
            item.snapshot = ItemSnapshot::Registry {
                entries: vec![RegSlot {
                    hive: Hive::Hkcu,
                    path: r"Software\Microsoft\GameBar".into(),
                    name: "AutoGameModeEnabled".into(),
                    value: None,
                }],
            };
        }
        revert_one(&mut sys, &backup, "p-gamemode", None).unwrap();
        assert_eq!(sys.reg_get(Hive::Hkcu, r"Software\Microsoft\GameBar", "AutoGameModeEnabled").unwrap(), None);
    }

    #[test]
    fn game_dvr_apply_and_revert_roundtrip() {
        let mut sys = ready();
        let backup = backup_of(&mut sys);
        apply_one(&mut sys, &ReadCache::empty(), &backup, "p-dvr", None).unwrap();
        let view = view_one(&mut sys, &ReadCache::empty(), Some(&backup), "p-dvr").unwrap();
        assert_eq!(view.state, ItemState::Ok);
        assert!(view.can_revert);
        revert_one(&mut sys, &backup, "p-dvr", None).unwrap();
        let again = view_one(&mut sys, &ReadCache::empty(), Some(&backup), "p-dvr").unwrap();
        assert_eq!(again.state, ItemState::CanImprove);
        assert!(!again.can_revert);
        assert_eq!(
            sys.reg_get(Hive::Hkcu, r"System\GameConfigStore", "GameDVR_Enabled").unwrap(),
            Some(RegValue::Dword(1))
        );
    }

    #[test]
    fn protection_disabled_requires_acknowledgement() {
        let mut sys = ready();
        sys.protection_enabled = false;
        let err = create_backup(&mut sys, false).unwrap_err();
        assert!(matches!(err, EngineError::ProtectionDisabled { .. }));
        let backup = create_backup(&mut sys, true).unwrap();
        assert_eq!(backup.restore_point.status, RestorePointStatus::ProtectionDisabled);
        assert!(backup.restore_point.acknowledged_no_protection);
        assert!(backup.items.len() >= 15);
    }

    #[test]
    fn existing_restore_point_inside_24h_is_reused() {
        let mut sys = ready();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        sys.now = Some(now);
        sys.add_restore_point_at("Windows Update", now - Duration::from_secs(3600));
        let before = sys.next_sequence;
        let backup = create_backup(&mut sys, false).unwrap();
        assert_eq!(backup.restore_point.status, RestorePointStatus::UsedExisting);
        assert_eq!(sys.next_sequence, before, "não pode criar outro ponto dentro de 24 h");
        assert!(backup.restore_point.message.contains("24 horas"));
    }

    #[test]
    fn old_restore_point_does_not_block_a_new_one() {
        let mut sys = ready();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        sys.now = Some(now);
        sys.add_restore_point_at("antigo", now - Duration::from_secs(25 * 3600));
        let backup = create_backup(&mut sys, false).unwrap();
        assert_eq!(backup.restore_point.status, RestorePointStatus::Created);
    }

    #[test]
    fn fresh_backup_creates_a_restore_point() {
        let mut sys = ready();
        let backup = create_backup(&mut sys, false).unwrap();
        assert_eq!(backup.restore_point.status, RestorePointStatus::Created);
        assert!(backup.restore_point.sequence.is_some());
    }

    #[test]
    fn mouse_accel_and_hotkeys_roundtrip() {
        let mut sys = ready();
        let backup = backup_of(&mut sys);
        apply_one(&mut sys, &ReadCache::empty(), &backup, "m-acel", None).unwrap();
        assert_eq!(sys.mouse_accel().unwrap(), MouseAccel { threshold1: 0, threshold2: 0, enabled: 0 });
        revert_one(&mut sys, &backup, "m-acel", None).unwrap();
        assert_eq!(sys.mouse_accel().unwrap(), MouseAccel { threshold1: 6, threshold2: 10, enabled: 1 });

        apply_one(&mut sys, &ReadCache::empty(), &backup, "k-atalhos", None).unwrap();
        assert_eq!(sys.sticky_flags().unwrap(), 506);
        assert_eq!(sys.filter_flags().unwrap() & HOTKEY_ACTIVE, 0);
        revert_one(&mut sys, &backup, "k-atalhos", None).unwrap();
        assert_eq!(sys.sticky_flags().unwrap(), 510);
    }

    #[test]
    fn directx_token_is_merged_and_revert_restores_the_string() {
        let mut sys = ready();
        sys.put(
            Hive::Hkcu,
            r"Software\Microsoft\DirectX\UserGpuPreferences",
            "DirectXUserGlobalSettings",
            RegValue::Sz("AutoHDREnable=1;".into()),
        );
        let backup = backup_of(&mut sys);
        apply_one(&mut sys, &ReadCache::empty(), &backup, "g-janela", None).unwrap();
        match sys.reg_get(Hive::Hkcu, r"Software\Microsoft\DirectX\UserGpuPreferences", "DirectXUserGlobalSettings").unwrap() {
            Some(RegValue::Sz(s)) => {
                assert!(s.contains("AutoHDREnable=1"));
                assert!(s.contains("SwapEffectUpgradeEnable=1"));
            }
            other => panic!("{other:?}"),
        }
        revert_one(&mut sys, &backup, "g-janela", None).unwrap();
        match sys.reg_get(Hive::Hkcu, r"Software\Microsoft\DirectX\UserGpuPreferences", "DirectXUserGlobalSettings").unwrap() {
            Some(RegValue::Sz(s)) => assert_eq!(s, "AutoHDREnable=1;"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn startup_never_deletes_run_entries_and_skips_protected() {
        let mut sys = ready();
        let backup = backup_of(&mut sys);
        let before = sys.startup_entries().unwrap();
        apply_one(&mut sys, &ReadCache::empty(), &backup, "i-run", None).unwrap();
        let after = sys.startup_entries().unwrap();
        assert_eq!(after.len(), before.len());
        let opera = after.iter().find(|e| e.name.contains("Opera")).unwrap();
        assert!(!opera.enabled());
        let health = after.iter().find(|e| e.name == "SecurityHealth").unwrap();
        assert!(health.enabled(), "Segurança do Windows continua");
        let audio = after.iter().find(|e| e.name == "RTHDVCPL").unwrap();
        assert!(audio.enabled(), "driver de áudio continua");
        revert_one(&mut sys, &backup, "i-run", None).unwrap();
        let restored = sys.startup_entries().unwrap();
        assert!(restored.iter().find(|e| e.name.contains("Opera")).unwrap().enabled());
    }

    #[test]
    fn spooler_is_not_touched_when_a_printer_exists() {
        let mut sys = ready();
        sys.printers = vec!["HP LaserJet".into()];
        let backup = backup_of(&mut sys);
        let err = apply_one(&mut sys, &ReadCache::empty(), &backup, "s-spooler", None).unwrap_err();
        assert!(err.to_string().contains("impressora"));
        assert_eq!(sys.service("Spooler").unwrap().start, StartType::Auto);
        let plan = plan_optimize(&sys, &ReadCache::empty(), Some(Section::Servicos)).unwrap();
        assert!(plan.local.is_empty() && plan.needs_admin.is_empty());
    }

    #[test]
    fn spooler_disable_and_revert_when_there_is_no_printer() {
        let mut sys = ready();
        let backup = backup_of(&mut sys);
        apply_one(&mut sys, &ReadCache::empty(), &backup, "s-spooler", None).unwrap();
        let svc = sys.service("Spooler").unwrap();
        assert_eq!(svc.start, StartType::Disabled);
        assert_eq!(svc.run, ServiceRun::Stopped);
        revert_one(&mut sys, &backup, "s-spooler", None).unwrap();
        let svc = sys.service("Spooler").unwrap();
        assert_eq!(svc.start, StartType::Auto);
        assert_eq!(svc.run, ServiceRun::Running);
    }

    #[test]
    fn sysmain_stays_out_of_section_optimize_and_refuses_without_evidence() {
        let mut sys = ready();
        let ctx = ReadCache { disk: Some(&sys.disk_report.clone()), disks: None, security: None };
        let plan = plan_optimize(&sys, &ctx, Some(Section::Disco)).unwrap();
        assert!(!plan.local.iter().any(|id| id == "d-sysmain"));
        assert!(!plan.needs_admin.iter().any(|id| id == "d-sysmain"));
        let backup = backup_of(&mut sys);
        let err = apply_one(&mut sys, &ctx, &backup, "d-sysmain", None).unwrap_err();
        assert!(err.to_string().contains("não altera"));
        assert_eq!(sys.service("SysMain").unwrap().start, StartType::Auto);
    }

    #[test]
    fn sysmain_can_be_parked_only_when_it_is_the_top_hdd_user() {
        let mut sys = ready();
        sys.disk_report.sysmain_is_top_on_hdd = true;
        let ctx = ReadCache { disk: Some(&sys.disk_report.clone()), disks: None, security: None };
        let backup = backup_of(&mut sys);
        apply_one(&mut sys, &ctx, &backup, "d-sysmain", None).unwrap();
        let svc = sys.service("SysMain").unwrap();
        assert_eq!(svc.start, StartType::Manual);
        assert_eq!(svc.run, ServiceRun::Stopped);
        revert_one(&mut sys, &backup, "d-sysmain", None).unwrap();
        let svc = sys.service("SysMain").unwrap();
        assert_eq!(svc.start, StartType::Auto);
        assert_eq!(svc.run, ServiceRun::Running);
    }

    #[test]
    fn power_plan_switches_balanced_and_keeps_ultimate() {
        let mut sys = ready();
        let view = view_one(&sys, &ReadCache::empty(), None, "p-energia").unwrap();
        assert_eq!(view.state, ItemState::Ok);
        sys.power = BALANCED_GUID.into();
        let backup = backup_of(&mut sys);
        apply_one(&mut sys, &ReadCache::empty(), &backup, "p-energia", None).unwrap();
        assert_eq!(normalize_guid(&sys.power_active().unwrap()), HIGH_PERFORMANCE_GUID);
        revert_one(&mut sys, &backup, "p-energia", None).unwrap();
        assert_eq!(normalize_guid(&sys.power_active().unwrap()), BALANCED_GUID);
    }

    #[test]
    fn failed_verify_restores_the_original() {
        let mut sys = ready();
        let backup = backup_of(&mut sys);
        sys.fail_writes = true;
        let err = apply_one(&mut sys, &ReadCache::empty(), &backup, "m-acel", None).unwrap_err();
        assert!(err.to_string().contains("restaurou"));
        sys.fail_writes = false;
        assert_eq!(sys.mouse_accel().unwrap().enabled, 1);
    }

    #[test]
    fn revert_all_puts_back_every_changed_item() {
        let mut sys = ready();
        let backup = backup_of(&mut sys);
        for id in ["p-dvr", "m-acel", "k-atalhos", "w-ads"] {
            apply_one(&mut sys, &ReadCache::empty(), &backup, id, None).unwrap();
        }
        let plan = plan_revert(&sys, &ReadCache::empty(), Some(&backup), None).unwrap();
        let ids = plan.local;
        assert!(ids.iter().any(|id| id == "p-dvr"));
        revert_ids(&mut sys, &backup, &ids, None);
        assert_eq!(
            sys.reg_get(Hive::Hkcu, r"System\GameConfigStore", "GameDVR_Enabled").unwrap(),
            Some(RegValue::Dword(1))
        );
        assert_eq!(sys.mouse_accel().unwrap().enabled, 1);
        assert_eq!(sys.sticky_flags().unwrap(), 510);
    }

    #[test]
    fn section_optimize_skips_readonly_and_conditional() {
        let sys = ready();
        let ctx = ReadCache { disk: Some(&sys.disk_report.clone()), disks: None, security: None };
        let plan = plan_optimize(&sys, &ctx, Some(Section::Disco)).unwrap();
        assert!(plan.local.is_empty());
        assert!(plan.needs_admin.is_empty());
        let plan = plan_optimize(&sys, &ctx, Some(Section::Desempenho)).unwrap();
        assert_eq!(plan.local, vec!["p-dvr".to_string()]);
    }

    #[test]
    fn unelevated_admin_item_is_not_applied() {
        let mut sys = ready();
        sys.elevated = false;
        let backup = backup_of(&mut sys);
        let err = apply_one(&mut sys, &ReadCache::empty(), &backup, "w-diag", None).unwrap_err();
        assert!(matches!(err, EngineError::NeedsAdmin));
        assert!(sys.reg_get(Hive::Hklm, r"SOFTWARE\Policies\Microsoft\Windows\DataCollection", "AllowTelemetry").unwrap().is_none());
    }

    #[test]
    fn edge_and_ads_revert_delete_values_that_did_not_exist() {
        let mut sys = ready();
        let backup = backup_of(&mut sys);
        apply_one(&mut sys, &ReadCache::empty(), &backup, "i-edge", None).unwrap();
        apply_one(&mut sys, &ReadCache::empty(), &backup, "w-ads", None).unwrap();
        revert_one(&mut sys, &backup, "i-edge", None).unwrap();
        revert_one(&mut sys, &backup, "w-ads", None).unwrap();
        assert!(sys.reg_get(Hive::Hklm, r"SOFTWARE\Policies\Microsoft\Edge", "StartupBoostEnabled").unwrap().is_none());
        match sys.reg_get(Hive::Hkcu, r"Software\Microsoft\Windows\CurrentVersion\AdvertisingInfo", "Enabled").unwrap() {
            Some(RegValue::Dword(1)) => {}
            other => panic!("anúncio deveria voltar a 1, veio {other:?}"),
        }
    }

    #[test]
    fn refresh_apply_sets_max_and_revert_restores() {
        let mut sys = ready();
        sys.display.monitors[0].hz = 60;
        sys.display.monitors[0].max_hz = 240;
        let backup = backup_of(&mut sys);
        apply_one(&mut sys, &ReadCache::empty(), &backup, "g-hz", None).unwrap();
        assert_eq!(sys.display().unwrap().monitors[0].hz, 240);
        revert_one(&mut sys, &backup, "g-hz", None).unwrap();
        assert_eq!(sys.display().unwrap().monitors[0].hz, 60);
    }

    #[test]
    fn backup_json_roundtrip_keeps_absent_values() {
        let mut sys = ready();
        let backup = backup_of(&mut sys);
        let dir = std::env::temp_dir().join(format!("turbo-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("backup.json");
        crate::store::save_backup(&path, &backup).unwrap();
        let loaded = crate::store::load_backup(&path).unwrap().unwrap();
        assert_eq!(loaded.items.len(), backup.items.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_records_apply_and_revert() {
        let mut sys = ready();
        let backup = backup_of(&mut sys);
        let dir = std::env::temp_dir().join(format!("turbo-log-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("log.jsonl");
        let _ = std::fs::remove_file(&path);
        apply_one(&mut sys, &ReadCache::empty(), &backup, "m-vel", Some(&path)).ok();
        // m-vel já está em 10, então não entra no plano. Força um valor diferente.
        sys.mouse_speed = 14;
        let backup = backup_of(&mut sys);
        apply_one(&mut sys, &ReadCache::empty(), &backup, "m-vel", Some(&path)).unwrap();
        revert_one(&mut sys, &backup, "m-vel", Some(&path)).unwrap();
        let entries = crate::store::read_log(&path).unwrap();
        assert!(entries.iter().any(|e| e.action == "apply" && e.ok && e.id == "m-vel"));
        assert!(entries.iter().any(|e| e.action == "revert" && e.ok && e.id == "m-vel"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
