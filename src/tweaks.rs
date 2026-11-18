//! Detectar, aplicar e reverter cada item da especificação.
//! Esta camada só fala com [`SystemBackend`].

use crate::backend::SystemBackend;
use crate::catalog::meta;
use crate::model::*;

pub struct ReadCache<'a> {
    pub disk: Option<&'a DiskReport>,
    pub disks: Option<&'a [DiskHealth]>,
    pub security: Option<&'a SecurityStatus>,
}

impl<'a> ReadCache<'a> {
    pub fn empty() -> Self {
        Self { disk: None, disks: None, security: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    pub state: ItemState,
    pub detail: String,
    /// Botão Otimizar. Itens condicionais só ligam isso quando a condição é verdadeira.
    pub offer_apply: bool,
}

pub fn diagnose(id: &str, sys: &dyn SystemBackend, ctx: &ReadCache<'_>) -> Result<Diagnosis, EngineError> {
    match id {
        "d-smart" => diag_smart(sys, ctx),
        "d-log" => diag_log(sys),
        "d-uso" => diag_uso(ctx),
        "d-onde" => diag_onde(sys),
        "d-sysmain" => diag_sysmain(sys, ctx),
        "p-dvr" => diag_dvr(sys),
        "p-gamemode" => diag_gamemode(sys),
        "p-energia" => diag_power(sys),
        "m-acel" => diag_mouse_accel(sys),
        "m-vel" => diag_mouse_speed(sys),
        "k-atalhos" => diag_hotkeys(sys),
        "g-hz" => diag_hz(sys),
        "g-janela" => diag_dx(sys),
        "n-eee" => diag_nic(sys),
        "n-link" => diag_link(sys),
        "s-spooler" => diag_spooler(sys),
        "i-run" => diag_startup(sys),
        "i-edge" => diag_edge(sys),
        "w-diag" => diag_telemetry(sys),
        "w-ads" => diag_ads(sys),
        "j-vanguard" => diag_vanguard(sys, ctx),
        _ => Err(EngineError::Message(format!("item desconhecido: {id}"))),
    }
}

pub fn capture(id: &str, sys: &dyn SystemBackend) -> Result<ItemSnapshot, EngineError> {
    match id {
        "d-sysmain" => capture_service(sys, "SysMain"),
        "s-spooler" => capture_service(sys, "Spooler"),
        "p-dvr" => Ok(ItemSnapshot::Registry { entries: read_slots(sys, &dvr_slots())? }),
        "p-gamemode" => Ok(ItemSnapshot::Registry { entries: read_slots(sys, &gamemode_slots())? }),
        "g-janela" => Ok(ItemSnapshot::Registry { entries: read_slots(sys, &dx_slots())? }),
        "i-edge" => Ok(ItemSnapshot::Registry { entries: read_slots(sys, &edge_slots())? }),
        "w-diag" => Ok(ItemSnapshot::Registry { entries: read_slots(sys, &telemetry_slots())? }),
        "w-ads" => Ok(ItemSnapshot::Registry { entries: read_slots(sys, &ads_slots())? }),
        "p-energia" => Ok(ItemSnapshot::Power { guid: normalize_guid(&sys.power_active()?) }),
        "m-acel" => {
            let m = sys.mouse_accel()?;
            Ok(ItemSnapshot::MouseAccel { threshold1: m.threshold1, threshold2: m.threshold2, enabled: m.enabled })
        }
        "m-vel" => Ok(ItemSnapshot::MouseSpeed { speed: sys.mouse_speed()? }),
        "k-atalhos" => Ok(ItemSnapshot::Hotkeys {
            sticky: sys.sticky_flags()?,
            filter: sys.filter_flags()?,
            toggle: sys.toggle_flags()?,
        }),
        "g-hz" => Ok(ItemSnapshot::Refresh { monitors: sys.display()?.monitors }),
        "n-eee" => capture_nic(sys),
        "i-run" => Ok(ItemSnapshot::Startup { entries: sys.startup_entries()? }),
        other if meta(other).map(|m| m.readonly).unwrap_or(false) => {
            Err(EngineError::ReadOnly)
        }
        other => Err(EngineError::Message(format!("item sem captura: {other}"))),
    }
}

pub fn apply(id: &str, sys: &mut dyn SystemBackend, ctx: &ReadCache<'_>) -> Result<String, EngineError> {
    match id {
        "d-sysmain" => apply_sysmain(sys, ctx),
        "s-spooler" => apply_spooler(sys),
        "p-dvr" => {
            write_dwords(sys, &dvr_targets())?;
            Ok("Gravação em segundo plano desligada.".into())
        }
        "p-gamemode" => {
            write_dword(sys, Hive::Hkcu, GAME_BAR, "AutoGameModeEnabled", 1)?;
            Ok("Modo de Jogo gravado como ligado.".into())
        }
        "p-energia" => apply_power(sys),
        "m-acel" => {
            sys.set_mouse_accel(MouseAccel { threshold1: 0, threshold2: 0, enabled: 0 })?;
            Ok("Aceleração do ponteiro desligada.".into())
        }
        "m-vel" => {
            sys.set_mouse_speed(10)?;
            Ok("Velocidade do ponteiro em 10.".into())
        }
        "k-atalhos" => apply_hotkeys(sys),
        "g-hz" => apply_hz(sys),
        "g-janela" => apply_dx(sys),
        "n-eee" => apply_nic(sys),
        "i-run" => apply_startup(sys),
        "i-edge" => {
            write_dwords(sys, &edge_targets())?;
            Ok("Políticas de segundo plano do Edge gravadas.".into())
        }
        "w-diag" => {
            write_dword(sys, Hive::Hklm, TELEMETRY, "AllowTelemetry", 1)?;
            Ok("Dados de diagnóstico limitados ao nível Obrigatório.".into())
        }
        "w-ads" => {
            write_dword(sys, Hive::Hkcu, ADS, "Enabled", 0)?;
            Ok("ID de anúncio desligado.".into())
        }
        other if meta(other).map(|m| m.readonly).unwrap_or(false) => Err(EngineError::ReadOnly),
        other => Err(EngineError::Message(format!("item sem aplicação: {other}"))),
    }
}

pub fn revert(id: &str, sys: &mut dyn SystemBackend, snap: &ItemSnapshot) -> Result<String, EngineError> {
    match (id, snap) {
        ("p-dvr" | "p-gamemode" | "g-janela" | "i-edge" | "w-diag" | "w-ads", ItemSnapshot::Registry { entries }) => {
            restore_slots(sys, entries)?;
            Ok("Valores originais do registro restaurados.".into())
        }
        ("p-energia", ItemSnapshot::Power { guid }) => {
            sys.power_set_active(guid)?;
            Ok(format!("Plano restaurado: {}.", power_name(guid)))
        }
        ("m-acel", ItemSnapshot::MouseAccel { threshold1, threshold2, enabled }) => {
            sys.set_mouse_accel(MouseAccel { threshold1: *threshold1, threshold2: *threshold2, enabled: *enabled })?;
            Ok("Aceleração do mouse restaurada.".into())
        }
        ("m-vel", ItemSnapshot::MouseSpeed { speed }) => {
            sys.set_mouse_speed(*speed)?;
            Ok(format!("Velocidade do ponteiro restaurada para {speed}."))
        }
        ("k-atalhos", ItemSnapshot::Hotkeys { sticky, filter, toggle }) => {
            sys.set_sticky_flags(*sticky)?;
            sys.set_filter_flags(*filter)?;
            sys.set_toggle_flags(*toggle)?;
            Ok("Atalhos de acessibilidade restaurados.".into())
        }
        ("g-hz", ItemSnapshot::Refresh { monitors }) => {
            for mon in monitors {
                sys.set_refresh(&mon.device, mon.hz)?;
            }
            Ok("Taxa de atualização restaurada.".into())
        }
        ("d-sysmain" | "s-spooler", ItemSnapshot::Service { name, running, start }) => {
            sys.set_service_start(name, *start)?;
            if *running {
                sys.start_service(name)?;
            } else if sys.service(name)?.run == ServiceRun::Running {
                sys.stop_service(name)?;
            }
            Ok(format!("{name} voltou para {}.", start.label()))
        }
        ("n-eee", ItemSnapshot::Nic { props }) => {
            for prop in props {
                sys.set_adapter_keyword(&prop.adapter, &prop.keyword, &prop.value)?;
            }
            Ok("Economia de energia da placa de rede restaurada.".into())
        }
        ("i-run", ItemSnapshot::Startup { entries }) => {
            for entry in entries {
                let (hive, path) = approved_location(entry.location);
                match &entry.approved {
                    Some(bytes) => sys.reg_set(hive, path, &entry.name, &RegValue::Binary(bytes.clone()))?,
                    None => sys.reg_delete(hive, path, &entry.name)?,
                }
            }
            Ok("Estado original da inicialização restaurado. Nenhuma entrada Run foi apagada.".into())
        }
        _ => Err(EngineError::Message("snapshot não combina com o item".into())),
    }
}

pub fn requires_admin(id: &str, sys: &dyn SystemBackend) -> Result<bool, EngineError> {
    if id == "i-run" {
        let entries = sys.startup_entries()?;
        return Ok(entries.iter().any(|e| {
            e.enabled() && !startup_protected(&e.name) && e.location.needs_admin()
        }));
    }
    Ok(meta(id).map(|m| m.needs_admin_apply).unwrap_or(false))
}

pub fn upsert_dx_token(existing: &str) -> String {
    upsert_token(existing, "SwapEffectUpgradeEnable", "1")
}

pub fn upsert_token(existing: &str, key: &str, value: &str) -> String {
    let prefix = format!("{key}=");
    let mut parts: Vec<String> = existing
        .split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    let replacement = format!("{key}={value}");
    if let Some(slot) = parts.iter_mut().find(|p| p.len() >= prefix.len() && p[..prefix.len()].eq_ignore_ascii_case(&prefix)) {
        *slot = replacement;
    } else {
        parts.push(replacement);
    }
    let mut joined = parts.join(";");
    if !joined.is_empty() {
        joined.push(';');
    }
    joined
}

const GAME_BAR: &str = r"Software\Microsoft\GameBar";
const GAME_DVR: &str = r"Software\Microsoft\Windows\CurrentVersion\GameDVR";
const GAME_STORE: &str = r"System\GameConfigStore";
const DX: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";
const EDGE: &str = r"SOFTWARE\Policies\Microsoft\Edge";
const TELEMETRY: &str = r"SOFTWARE\Policies\Microsoft\Windows\DataCollection";
const ADS: &str = r"Software\Microsoft\Windows\CurrentVersion\AdvertisingInfo";

fn dvr_slots() -> Vec<(Hive, &'static str, &'static str)> {
    vec![
        (Hive::Hkcu, GAME_STORE, "GameDVR_Enabled"),
        (Hive::Hkcu, GAME_DVR, "AppCaptureEnabled"),
        (Hive::Hkcu, GAME_DVR, "HistoricalCaptureEnabled"),
    ]
}

fn dvr_targets() -> Vec<(Hive, &'static str, &'static str, u32)> {
    dvr_slots().into_iter().map(|(h, p, n)| (h, p, n, 0)).collect()
}

fn gamemode_slots() -> Vec<(Hive, &'static str, &'static str)> {
    vec![(Hive::Hkcu, GAME_BAR, "AutoGameModeEnabled")]
}

fn dx_slots() -> Vec<(Hive, &'static str, &'static str)> {
    vec![(Hive::Hkcu, DX, "DirectXUserGlobalSettings")]
}

fn edge_slots() -> Vec<(Hive, &'static str, &'static str)> {
    vec![
        (Hive::Hklm, EDGE, "StartupBoostEnabled"),
        (Hive::Hklm, EDGE, "BackgroundModeEnabled"),
    ]
}

fn edge_targets() -> Vec<(Hive, &'static str, &'static str, u32)> {
    edge_slots().into_iter().map(|(h, p, n)| (h, p, n, 0)).collect()
}

fn telemetry_slots() -> Vec<(Hive, &'static str, &'static str)> {
    vec![(Hive::Hklm, TELEMETRY, "AllowTelemetry")]
}

fn ads_slots() -> Vec<(Hive, &'static str, &'static str)> {
    vec![(Hive::Hkcu, ADS, "Enabled")]
}

fn approved_location(location: StartupLocation) -> (Hive, &'static str) {
    match location {
        StartupLocation::HkcuRun => (Hive::Hkcu, r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"),
        StartupLocation::HklmRun => (Hive::Hklm, r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"),
        StartupLocation::HklmRun32 => (Hive::Hklm, r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run32"),
        StartupLocation::UserStartupFolder => (Hive::Hkcu, r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder"),
        StartupLocation::CommonStartupFolder => (Hive::Hklm, r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder"),
    }
}

fn read_slots(sys: &dyn SystemBackend, spec: &[(Hive, &str, &str)]) -> Result<Vec<RegSlot>, EngineError> {
    let mut out = Vec::new();
    for (hive, path, name) in spec {
        out.push(RegSlot {
            hive: *hive,
            path: (*path).to_string(),
            name: (*name).to_string(),
            value: sys.reg_get(*hive, path, name)?,
        });
    }
    Ok(out)
}

fn restore_slots(sys: &mut dyn SystemBackend, entries: &[RegSlot]) -> Result<(), EngineError> {
    for slot in entries {
        match &slot.value {
            Some(value) => sys.reg_set(slot.hive, &slot.path, &slot.name, value)?,
            None => sys.reg_delete(slot.hive, &slot.path, &slot.name)?,
        }
    }
    Ok(())
}

fn write_dword(sys: &mut dyn SystemBackend, hive: Hive, path: &str, name: &str, value: u32) -> Result<(), EngineError> {
    sys.reg_set(hive, path, name, &RegValue::Dword(value))?;
    Ok(())
}

fn write_dwords(sys: &mut dyn SystemBackend, rows: &[(Hive, &str, &str, u32)]) -> Result<(), EngineError> {
    for (hive, path, name, value) in rows {
        write_dword(sys, *hive, path, name, *value)?;
    }
    Ok(())
}

fn dword(sys: &dyn SystemBackend, hive: Hive, path: &str, name: &str) -> Result<Option<u32>, EngineError> {
    match sys.reg_get(hive, path, name)? {
        None => Ok(None),
        Some(RegValue::Dword(v)) => Ok(Some(v)),
        Some(_) => Err(EngineError::Message(format!("{path}\\{name} não é DWORD"))),
    }
}

fn from_denied(err: BackendError) -> (bool, EngineError) {
    match err {
        BackendError::AccessDenied(msg) => (true, EngineError::Message(msg)),
        other => (false, other.into()),
    }
}

fn diag_smart(sys: &dyn SystemBackend, ctx: &ReadCache<'_>) -> Result<Diagnosis, EngineError> {
    let disks = match sys.disks() {
        Ok(d) => d,
        Err(err) => {
            let (denied, mapped) = from_denied(err);
            if denied {
                if let Some(cached) = ctx.disks {
                    return Ok(judge_disks(cached));
                }
                return Ok(Diagnosis {
                    state: ItemState::NeedsAdmin,
                    detail: "Os contadores SMART pedem administrador. Use \"Ler como admin\".".into(),
                    offer_apply: false,
                });
            }
            return Err(mapped);
        }
    };
    Ok(judge_disks(&disks))
}

fn judge_disks(disks: &[DiskHealth]) -> Diagnosis {
    if disks.is_empty() {
        return Diagnosis { state: ItemState::NotRead, detail: "Nenhum disco físico foi retornado.".into(), offer_apply: false };
    }
    let mut problems = Vec::new();
    let mut lines = Vec::new();
    for disk in disks {
        let wear = disk.wear.map(|w| format!("{w}%")).unwrap_or_else(|| "n/d".into());
        let temp = match (disk.temp_c, disk.temp_max_c) {
            (Some(t), Some(m)) => format!("{t} °C / máx {m} °C"),
            (Some(t), None) => format!("{t} °C"),
            _ => "temperatura n/d".into(),
        };
        lines.push(format!(
            "{} ({}, {}): {} · {} · desgaste {wear} · erros L/E {}/{}",
            disk.name,
            disk.media.label(),
            disk.bus,
            disk.health,
            temp,
            disk.read_errors.map(|n| n.to_string()).unwrap_or_else(|| "n/d".into()),
            disk.write_errors.map(|n| n.to_string()).unwrap_or_else(|| "n/d".into()),
        ));
        if !disk.health.eq_ignore_ascii_case("Healthy") {
            problems.push(format!("{} não está saudável ({})", disk.name, disk.health));
        }
        if disk.read_errors.unwrap_or(0) > 0 || disk.write_errors.unwrap_or(0) > 0 {
            problems.push(format!("{} tem erros de leitura ou escrita não corrigidos", disk.name));
        }
        if let (Some(t), Some(max)) = (disk.temp_c, disk.temp_max_c) {
            if max > 0 && t >= max {
                problems.push(format!("{} está na temperatura máxima", disk.name));
            }
        }
        if disk.wear.unwrap_or(0) >= 100 {
            problems.push(format!("{} chegou a 100% de desgaste", disk.name));
        }
        if disk.read_errors.is_none() || disk.write_errors.is_none() {
            problems.push(format!("contadores SMART de {} não vieram", disk.name));
        }
    }
    let detail = lines.join("\n");
    if problems.is_empty() {
        Diagnosis { state: ItemState::Ok, detail, offer_apply: false }
    } else {
        Diagnosis {
            state: ItemState::CanImprove,
            detail: format!("{detail}\n{}", problems.join("; ")),
            offer_apply: false,
        }
    }
}

fn diag_log(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let events = sys.disk_events()?;
    if events.is_empty() {
        return Ok(Diagnosis {
            state: ItemState::Ok,
            detail: "Nenhum evento 7, 51, 129 ou 153 nos últimos 7 dias.".into(),
            offer_apply: false,
        });
    }
    let lines: Vec<String> = events.iter().map(|e| format!("{}× evento {} ({}) {}", e.count, e.id, e.provider, e.sample)).collect();
    Ok(Diagnosis {
        state: ItemState::CanImprove,
        detail: lines.join("\n"),
        offer_apply: false,
    })
}

fn diag_uso(ctx: &ReadCache<'_>) -> Result<Diagnosis, EngineError> {
    let Some(report) = ctx.disk else {
        return Ok(Diagnosis {
            state: ItemState::NotRead,
            detail: "Ainda não medido. A leitura leva 10 segundos e não altera nada.".into(),
            offer_apply: false,
        });
    };
    let mut lines = Vec::new();
    let mut hot = false;
    for disk in &report.disks {
        lines.push(format!(
            "{} ({}): {:.0}% ocupado, fila {:.2}, {:.1} ms/transferência",
            disk.name,
            disk.media.label(),
            disk.busy_percent,
            disk.queue,
            disk.sec_per_transfer * 1000.0
        ));
        if disk.busy_percent >= 80.0 || disk.queue >= 2.0 {
            hot = true;
        }
    }
    if !report.top_processes.is_empty() {
        lines.push("Processos com mais I/O (indicativo, inclui rede):".into());
        for proc in report.top_processes.iter().take(5) {
            lines.push(format!("  {} — {:.0} B/s", proc.name, proc.bytes_per_sec));
        }
    }
    Ok(Diagnosis {
        state: if hot { ItemState::CanImprove } else { ItemState::Ok },
        detail: if lines.is_empty() { "Sem contadores de disco.".into() } else { lines.join("\n") },
        offer_apply: false,
    })
}

fn diag_onde(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let loc = sys.windows_location()?;
    let ok = loc.system_media.is_solid() && loc.pagefile_media.is_solid();
    let detail = format!(
        "Windows em {} ({}, {}). Pagefile {} ({} MB, {}).",
        loc.system_drive,
        loc.system_disk,
        loc.system_media.label(),
        loc.pagefile_path,
        loc.pagefile_size_mb,
        loc.pagefile_media.label()
    );
    let detail = if ok {
        detail
    } else {
        format!("{detail}\nWindows ou pagefile num HD mecânico deixa o disco a 100%. Mover o sistema não é um botão deste app.")
    };
    Ok(Diagnosis {
        state: if ok { ItemState::Ok } else { ItemState::CanImprove },
        detail,
        offer_apply: false,
    })
}

fn diag_sysmain(sys: &dyn SystemBackend, ctx: &ReadCache<'_>) -> Result<Diagnosis, EngineError> {
    let svc = sys.service("SysMain")?;
    let parked = svc.start == StartType::Manual && svc.run == ServiceRun::Stopped;
    if parked {
        return Ok(Diagnosis {
            state: ItemState::Ok,
            detail: "SysMain está em Manual e parado.".into(),
            offer_apply: false,
        });
    }
    let Some(report) = ctx.disk else {
        return Ok(Diagnosis {
            state: ItemState::NotRead,
            detail: format!("SysMain está {} / {}. A medição de disco ainda não rodou, então o botão fica escondido.", svc.run_label(), svc.start.label()),
            offer_apply: false,
        });
    };
    if report.sysmain_is_top_on_hdd {
        Ok(Diagnosis {
            state: ItemState::CanImprove,
            detail: format!("SysMain ({}/{}) é o maior usuário de um HD. Dá para passar o serviço para Manual.", svc.run_label(), svc.start.label()),
            offer_apply: true,
        })
    } else {
        Ok(Diagnosis {
            state: ItemState::Ok,
            detail: format!("SysMain está {} / {} e não lidera o uso de um HD. O app não mexe.", svc.run_label(), svc.start.label()),
            offer_apply: false,
        })
    }
}

fn diag_dvr(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let game = dword(sys, Hive::Hkcu, GAME_STORE, "GameDVR_Enabled")?;
    let app = dword(sys, Hive::Hkcu, GAME_DVR, "AppCaptureEnabled")?;
    let hist = dword(sys, Hive::Hkcu, GAME_DVR, "HistoricalCaptureEnabled")?;
    let ok = game == Some(0) && app == Some(0) && hist == Some(0);
    let detail = format!(
        "GameDVR_Enabled={}, AppCaptureEnabled={}, HistoricalCaptureEnabled={}.",
        fmt_opt(game),
        fmt_opt(app),
        fmt_opt(hist)
    );
    Ok(Diagnosis {
        state: if ok { ItemState::Ok } else { ItemState::CanImprove },
        detail,
        offer_apply: !ok,
    })
}

fn diag_gamemode(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let value = dword(sys, Hive::Hkcu, GAME_BAR, "AutoGameModeEnabled")?;
    let ok = value.is_none() || value == Some(1);
    let detail = match value {
        None => "AutoGameModeEnabled ausente: o padrão do Windows é Modo de Jogo ligado.".into(),
        Some(v) => format!("AutoGameModeEnabled = {v}."),
    };
    Ok(Diagnosis {
        state: if ok { ItemState::Ok } else { ItemState::CanImprove },
        detail,
        offer_apply: !ok,
    })
}

fn diag_power(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let guid = sys.power_active()?;
    let ok = is_high_or_ultimate(&guid);
    Ok(Diagnosis {
        state: if ok { ItemState::Ok } else { ItemState::CanImprove },
        detail: format!("Plano ativo: {}.", power_name(&guid)),
        offer_apply: !ok,
    })
}

fn diag_mouse_accel(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let m = sys.mouse_accel()?;
    let ok = m.threshold1 == 0 && m.threshold2 == 0 && m.enabled == 0;
    Ok(Diagnosis {
        state: if ok { ItemState::Ok } else { ItemState::CanImprove },
        detail: format!("Limiar 1 = {}, limiar 2 = {}, aceleração = {}.", m.threshold1, m.threshold2, m.enabled),
        offer_apply: !ok,
    })
}

fn diag_mouse_speed(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let speed = sys.mouse_speed()?;
    let ok = speed == 10;
    Ok(Diagnosis {
        state: if ok { ItemState::Ok } else { ItemState::CanImprove },
        detail: format!("MouseSensitivity = {speed}."),
        offer_apply: !ok,
    })
}

fn diag_hotkeys(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let sticky = sys.sticky_flags()?;
    let filter = sys.filter_flags()?;
    let toggle = sys.toggle_flags()?;
    let hot = |f: u32| f & HOTKEY_ACTIVE != 0;
    let ok = !hot(sticky) && !hot(filter) && !hot(toggle);
    Ok(Diagnosis {
        state: if ok { ItemState::Ok } else { ItemState::CanImprove },
        detail: format!("Aderência {sticky}, Filtragem {filter}, Alternância {toggle}. O bit de atalho é 0x4."),
        offer_apply: !ok,
    })
}

fn diag_hz(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let info = sys.display()?;
    if info.monitors.is_empty() {
        return Ok(Diagnosis { state: ItemState::NotRead, detail: "Nenhum monitor encontrado.".into(), offer_apply: false });
    }
    let mut lines = Vec::new();
    let mut bad = false;
    for mon in &info.monitors {
        let name = if mon.label.is_empty() { "Monitor" } else { &mon.label };
        lines.push(format!("{name}: {}×{} a {} Hz (máximo nesta resolução: {} Hz).", mon.width, mon.height, mon.hz, mon.max_hz));
        if mon.max_hz > 0 && mon.hz < mon.max_hz {
            bad = true;
        }
    }
    Ok(Diagnosis {
        state: if bad { ItemState::CanImprove } else { ItemState::Ok },
        detail: lines.join("\n"),
        offer_apply: bad,
    })
}

fn diag_dx(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let current = match sys.reg_get(Hive::Hkcu, DX, "DirectXUserGlobalSettings")? {
        None => String::new(),
        Some(RegValue::Sz(s)) => s,
        Some(_) => return Err(EngineError::Message("DirectXUserGlobalSettings não é texto".into())),
    };
    let ok = token_is(&current, "SwapEffectUpgradeEnable", "1");
    let detail = if current.is_empty() {
        "DirectXUserGlobalSettings não definido. O app grava só SwapEffectUpgradeEnable=1.".into()
    } else {
        format!("Valor atual: {current}")
    };
    Ok(Diagnosis {
        state: if ok { ItemState::Ok } else { ItemState::CanImprove },
        detail,
        offer_apply: !ok,
    })
}

fn token_is(existing: &str, key: &str, value: &str) -> bool {
    let prefix = format!("{key}=");
    existing.split(';').map(str::trim).any(|part| {
        part.len() >= prefix.len()
            && part[..prefix.len()].eq_ignore_ascii_case(&prefix)
            && part[prefix.len()..].eq_ignore_ascii_case(value)
    })
}

fn nic_power_name(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "energia eficiente ethernet",
        "energy-efficient ethernet",
        "energy efficient ethernet",
        "power saving mode",
        "green ethernet",
    ];
    NAMES.iter().any(|n| name.eq_ignore_ascii_case(n))
}

fn wired_power_props(adapters: &[AdapterInfo]) -> Vec<(&str, &NicPowerProp)> {
    adapters
        .iter()
        .filter(|a| a.wired)
        .flat_map(|a| a.power_props.iter().filter(|p| nic_power_name(&p.display_name)).map(move |p| (a.name.as_str(), p)))
        .collect()
}

fn diag_nic(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let adapters = sys.adapters()?;
    let wired: Vec<&AdapterInfo> = adapters.iter().filter(|a| a.wired).collect();
    if wired.is_empty() {
        return Ok(Diagnosis {
            state: ItemState::Ok,
            detail: "Nenhuma placa cabeada encontrada. O item não se aplica.".into(),
            offer_apply: false,
        });
    }
    let props = wired_power_props(&adapters);
    if props.is_empty() {
        return Ok(Diagnosis {
            state: ItemState::Ok,
            detail: "A placa cabeada não expõe Energia Eficiente Ethernet, Power Saving Mode nem Green Ethernet.".into(),
            offer_apply: false,
        });
    }
    let mut lines = Vec::new();
    let mut bad = false;
    for (adapter, prop) in &props {
        let off = prop.current == prop.disabled_value
            || prop.current_display.eq_ignore_ascii_case("desativado")
            || prop.current_display.eq_ignore_ascii_case("disabled")
            || prop.current_display.eq_ignore_ascii_case("off");
        if !off {
            bad = true;
        }
        lines.push(format!("{adapter} · {} ({}) = {}", prop.display_name, prop.keyword, prop.current_display));
    }
    Ok(Diagnosis {
        state: if bad { ItemState::CanImprove } else { ItemState::Ok },
        detail: lines.join("\n"),
        offer_apply: bad,
    })
}

fn diag_link(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let adapters = sys.adapters()?;
    if adapters.is_empty() {
        return Ok(Diagnosis { state: ItemState::NotRead, detail: "Nenhuma placa de rede encontrada.".into(), offer_apply: false });
    }
    let mut lines = Vec::new();
    let mut bad = false;
    for adapter in adapters.iter().filter(|a| a.connected) {
        let kind = if adapter.wired { "cabo" } else { "sem fio" };
        lines.push(format!(
            "{} ({kind}): link {}, máxima {}.",
            adapter.name,
            bps_label(adapter.link_bps),
            bps_label(adapter.max_bps)
        ));
        if adapter.wired && adapter.max_bps > 0 && adapter.link_bps > 0 && adapter.link_bps < adapter.max_bps {
            bad = true;
        }
    }
    if lines.is_empty() {
        lines.push("Nenhuma placa conectada.".into());
    }
    Ok(Diagnosis {
        state: if bad { ItemState::CanImprove } else { ItemState::Ok },
        detail: lines.join("\n"),
        offer_apply: false,
    })
}

fn diag_spooler(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let printers = sys.physical_printers()?;
    let svc = sys.service("Spooler")?;
    if printers.is_empty() {
        let ok = svc.start == StartType::Disabled && svc.run == ServiceRun::Stopped;
        return Ok(Diagnosis {
            state: if ok { ItemState::Ok } else { ItemState::CanImprove },
            detail: format!("Nenhuma impressora física. Spooler {} / {}.", svc.run_label(), svc.start.label()),
            offer_apply: !ok,
        });
    }
    let ok = svc.run == ServiceRun::Running && svc.start != StartType::Disabled;
    Ok(Diagnosis {
        state: if ok { ItemState::Ok } else { ItemState::CanImprove },
        detail: format!(
            "Impressoras: {}. Spooler {} / {}. O botão de desligar não aparece quando existe impressora.",
            printers.join(", "),
            svc.run_label(),
            svc.start.label()
        ),
        offer_apply: false,
    })
}

fn diag_startup(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let entries = sys.startup_entries()?;
    if entries.is_empty() {
        return Ok(Diagnosis { state: ItemState::Ok, detail: "Nenhum item de inicialização encontrado.".into(), offer_apply: true });
    }
    let mut lines = Vec::new();
    let mut actionable = 0;
    for entry in &entries {
        let protected = startup_protected(&entry.name);
        let flag = if protected {
            "protegido"
        } else if entry.enabled() {
            actionable += 1;
            "pode desativar"
        } else {
            "desativado"
        };
        lines.push(format!("{} [{}] — {flag} — {}", entry.name, entry.location.label(), entry.command));
    }
    Ok(Diagnosis {
        state: if actionable == 0 { ItemState::Ok } else { ItemState::CanImprove },
        detail: lines.join("\n"),
        offer_apply: actionable > 0,
    })
}

fn diag_edge(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let boost = dword(sys, Hive::Hklm, EDGE, "StartupBoostEnabled")?;
    let bg = dword(sys, Hive::Hklm, EDGE, "BackgroundModeEnabled")?;
    let ok = boost == Some(0) && bg == Some(0);
    Ok(Diagnosis {
        state: if ok { ItemState::Ok } else { ItemState::CanImprove },
        detail: format!("StartupBoostEnabled={}, BackgroundModeEnabled={}.", fmt_opt(boost), fmt_opt(bg)),
        offer_apply: !ok,
    })
}

fn diag_telemetry(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let value = dword(sys, Hive::Hklm, TELEMETRY, "AllowTelemetry")?;
    let ok = value == Some(1);
    let detail = match value {
        None => "AllowTelemetry ausente. O app grava 1 (Obrigatório), o mínimo do Windows Pro.".into(),
        Some(v) => format!("AllowTelemetry = {v}."),
    };
    Ok(Diagnosis {
        state: if ok { ItemState::Ok } else { ItemState::CanImprove },
        detail,
        offer_apply: !ok,
    })
}

fn diag_ads(sys: &dyn SystemBackend) -> Result<Diagnosis, EngineError> {
    let value = dword(sys, Hive::Hkcu, ADS, "Enabled")?;
    let ok = value == Some(0);
    Ok(Diagnosis {
        state: if ok { ItemState::Ok } else { ItemState::CanImprove },
        detail: format!("AdvertisingInfo Enabled = {}.", fmt_opt(value)),
        offer_apply: !ok,
    })
}

fn diag_vanguard(sys: &dyn SystemBackend, ctx: &ReadCache<'_>) -> Result<Diagnosis, EngineError> {
    let status = match sys.security_status() {
        Ok(s) => s,
        Err(err) => {
            let (denied, mapped) = from_denied(err);
            if denied {
                if let Some(cached) = ctx.security {
                    return Ok(judge_security(cached));
                }
                return Ok(Diagnosis {
                    state: ItemState::NeedsAdmin,
                    detail: "Ler TPM e Secure Boot pede administrador. O app não altera nenhum dos dois.".into(),
                    offer_apply: false,
                });
            }
            return Err(mapped);
        }
    };
    Ok(judge_security(&status))
}

fn judge_security(status: &SecurityStatus) -> Diagnosis {
    let version_ok = status.spec_version.split([',', ' ']).any(|p| p == "2.0");
    let ok = status.secure_boot == Some(true)
        && status.tpm_present == Some(true)
        && status.tpm_ready == Some(true)
        && version_ok;
    let detail = format!(
        "Secure Boot: {}. TPM presente: {}. TPM pronto: {}. SpecVersion: {}.",
        fmt_bool(status.secure_boot),
        fmt_bool(status.tpm_present),
        fmt_bool(status.tpm_ready),
        if status.spec_version.is_empty() { "n/d" } else { &status.spec_version }
    );
    let detail = if ok {
        detail
    } else {
        format!("{detail}\nSem TPM 2.0 ou Secure Boot o Valorant não abre no Windows 11 (VAN9001 / VAN9003). O Turbo não mexe nisso. Guia da Riot: https://support-valorant.riotgames.com/hc/en-us/articles/48069730092051-TPM-2-0-Guide")
    };
    Diagnosis {
        state: if ok { ItemState::Ok } else { ItemState::CanImprove },
        detail,
        offer_apply: false,
    }
}

fn apply_sysmain(sys: &mut dyn SystemBackend, ctx: &ReadCache<'_>) -> Result<String, EngineError> {
    let diag = diag_sysmain(sys, ctx)?;
    if !diag.offer_apply {
        return Err(EngineError::Message("SysMain não é o maior usuário de um HD. O Turbo não altera esse serviço.".into()));
    }
    sys.stop_service("SysMain")?;
    sys.set_service_start("SysMain", StartType::Manual)?;
    Ok("SysMain parado e colocado em Manual.".into())
}

fn apply_spooler(sys: &mut dyn SystemBackend) -> Result<String, EngineError> {
    let printers = sys.physical_printers()?;
    if !printers.is_empty() {
        return Err(EngineError::Message("Há impressora física. O Turbo não desliga o Spooler.".into()));
    }
    if sys.service("Spooler")?.run == ServiceRun::Running {
        sys.stop_service("Spooler")?;
    }
    sys.set_service_start("Spooler", StartType::Disabled)?;
    Ok("Spooler parado e desativado.".into())
}

fn apply_power(sys: &mut dyn SystemBackend) -> Result<String, EngineError> {
    let current = sys.power_active()?;
    if is_high_or_ultimate(&current) {
        return Ok(format!("O plano já é {}.", power_name(&current)));
    }
    if !is_saver_or_balanced(&current) {
        // Plano personalizado não está na lista de OK. A spec manda o alto desempenho
        // quando o ativo não é Alto desempenho nem Desempenho Máximo.
    }
    sys.power_set_active(HIGH_PERFORMANCE_GUID)?;
    Ok("Plano Alto desempenho ativado.".into())
}

fn apply_hotkeys(sys: &mut dyn SystemBackend) -> Result<String, EngineError> {
    let sticky = sys.sticky_flags()? & !HOTKEY_ACTIVE;
    let filter = sys.filter_flags()? & !HOTKEY_ACTIVE;
    let toggle = sys.toggle_flags()? & !HOTKEY_ACTIVE;
    sys.set_sticky_flags(sticky)?;
    sys.set_filter_flags(filter)?;
    sys.set_toggle_flags(toggle)?;
    Ok(format!("Atalhos desligados. Aderência agora {sticky}."))
}

fn apply_hz(sys: &mut dyn SystemBackend) -> Result<String, EngineError> {
    let info = sys.display()?;
    let mut changed = 0;
    for mon in &info.monitors {
        if mon.max_hz > mon.hz {
            sys.set_refresh(&mon.device, mon.max_hz)?;
            changed += 1;
        }
    }
    if changed == 0 {
        Ok("Os monitores já estão na maior taxa da resolução atual.".into())
    } else {
        Ok(format!("{changed} monitor(es) na maior taxa. Confirme a imagem em 15 segundos."))
    }
}

fn apply_dx(sys: &mut dyn SystemBackend) -> Result<String, EngineError> {
    let current = match sys.reg_get(Hive::Hkcu, DX, "DirectXUserGlobalSettings")? {
        Some(RegValue::Sz(s)) => s,
        _ => String::new(),
    };
    let next = upsert_dx_token(&current);
    sys.reg_set(Hive::Hkcu, DX, "DirectXUserGlobalSettings", &RegValue::Sz(next))?;
    Ok("SwapEffectUpgradeEnable=1 gravado, sem apagar os outros tokens.".into())
}

fn apply_nic(sys: &mut dyn SystemBackend) -> Result<String, EngineError> {
    let adapters = sys.adapters()?;
    let mut n = 0;
    let props: Vec<(String, String, String)> = wired_power_props(&adapters)
        .into_iter()
        .filter(|(_, p)| p.current != p.disabled_value)
        .map(|(a, p)| (a.to_string(), p.keyword.clone(), p.disabled_value.clone()))
        .collect();
    for (adapter, keyword, value) in props {
        sys.set_adapter_keyword(&adapter, &keyword, &value)?;
        n += 1;
    }
    if n == 0 {
        Ok("Nada para desligar na placa cabeada.".into())
    } else {
        Ok(format!("{n} propriedade(s) de economia de energia desligada(s). A rede pode cair por alguns segundos."))
    }
}

fn apply_startup(sys: &mut dyn SystemBackend) -> Result<String, EngineError> {
    let entries = sys.startup_entries()?;
    let mut n = 0;
    for entry in entries {
        if startup_protected(&entry.name) || !entry.enabled() {
            continue;
        }
        let (hive, path) = approved_location(entry.location);
        sys.reg_set(hive, path, &entry.name, &RegValue::Binary(startup_disabled_blob()))?;
        n += 1;
    }
    Ok(format!("{n} item(ns) desativado(s) na inicialização. As entradas Run continuam no registro."))
}

fn capture_service(sys: &dyn SystemBackend, name: &str) -> Result<ItemSnapshot, EngineError> {
    let svc = sys.service(name)?;
    Ok(ItemSnapshot::Service {
        name: name.to_string(),
        running: svc.run == ServiceRun::Running,
        start: svc.start,
    })
}

fn capture_nic(sys: &dyn SystemBackend) -> Result<ItemSnapshot, EngineError> {
    let adapters = sys.adapters()?;
    let props = wired_power_props(&adapters)
        .into_iter()
        .map(|(adapter, prop)| NicSnap {
            adapter: adapter.to_string(),
            keyword: prop.keyword.clone(),
            value: prop.current.clone(),
        })
        .collect();
    Ok(ItemSnapshot::Nic { props })
}

fn fmt_opt(value: Option<u32>) -> String {
    match value {
        Some(v) => v.to_string(),
        None => "ausente".into(),
    }
}

fn fmt_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "sim",
        Some(false) => "não",
        None => "n/d",
    }
}

trait RunLabel {
    fn run_label(&self) -> &'static str;
}

impl RunLabel for ServiceInfo {
    fn run_label(&self) -> &'static str {
        match self.run {
            ServiceRun::Running => "em execução",
            ServiceRun::Stopped => "parado",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_keeps_the_other_settings() {
        let next = upsert_token("AutoHDREnable=1;SwapEffectUpgradeEnable=0;", "SwapEffectUpgradeEnable", "1");
        assert_eq!(next, "AutoHDREnable=1;SwapEffectUpgradeEnable=1;");
        let added = upsert_token("", "SwapEffectUpgradeEnable", "1");
        assert_eq!(added, "SwapEffectUpgradeEnable=1;");
    }

    #[test]
    fn sticky_mask_clears_only_the_hotkey_bit() {
        assert_eq!(510 & !HOTKEY_ACTIVE, 506);
    }
}
