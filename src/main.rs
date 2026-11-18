//! Interface do Turbo. O motor testável fica na biblioteca.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slint::{ComponentHandle, ModelRc, VecModel};
use turbo::backend::SystemBackend;
use turbo::catalog::{self, meta};
use turbo::engine::{self, ApplyOutcome};
use turbo::fake::FakeBackend;
use turbo::model::{BackupFile, ItemState, ItemView, Section, APP_VERSION};
use turbo::store::{self, AdminCacheFile};
use turbo::tweaks::ReadCache;
use turbo::updater::{self, UpdateDecision, UpdateManifest};

slint::include_modules!();

enum Host {
    Demo(Mutex<FakeBackend>),
    #[cfg(windows)]
    Windows,
}

struct Session {
    backup: Option<BackupFile>,
    disk: Option<turbo::model::DiskReport>,
    admin: AdminCacheFile,
    views: Vec<ItemView>,
    pending: Pending,
    resume: Option<FollowUp>,
    hz_token: u64,
}

#[derive(Clone)]
enum Pending {
    None,
    AckProtection,
    NeedBackup(FollowUp),
    Confirm(FollowUp),
    Hz { token: u64 },
}

#[derive(Clone)]
enum FollowUp {
    One(String),
    Section,
    RevertOne(String),
    RevertSection,
    RevertAll,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("Turbo {APP_VERSION}");
        return;
    }
    if let Some(path) = flag(&args, "--apply") {
        let result = flag(&args, "--result");
        std::process::exit(run_apply_batch(Path::new(&path), result.as_deref()));
    }
    if flag(&args, "--read-admin").is_some() {
        let result = flag(&args, "--result").unwrap_or_else(|| store::admin_cache_path().display().to_string());
        std::process::exit(run_admin_read(Path::new(&result)));
    }
    if let Some(setup) = flag(&args, "--launch-installer") {
        let pid = args.iter().position(|a| a == "--launch-installer").and_then(|i| args.get(i + 2)).and_then(|s| s.parse().ok()).unwrap_or(0);
        wait_and_launch(Path::new(&setup), pid);
        return;
    }
    ui_main();
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn ui_main() {
    let _ = std::fs::create_dir_all(store::data_dir());
    let backup = store::load_backup(&store::backup_path()).ok().flatten();
    let admin = store::load_admin_cache(&store::admin_cache_path());
    #[cfg(windows)]
    let host = Arc::new(Host::Windows);
    #[cfg(not(windows))]
    let host = Arc::new(Host::Demo(Mutex::new(FakeBackend::carlos_demo())));

    let session = Arc::new(Mutex::new(Session {
        backup,
        disk: None,
        admin,
        views: Vec::new(),
        pending: Pending::None,
        resume: None,
        hz_token: 0,
    }));

    let ui = MainWindow::new().expect("criar a janela");
    ui.set_version_label(APP_VERSION.into());
    #[cfg(not(windows))]
    ui.set_banner("Demonstração no Linux: os dados são de exemplo e nada é alterado no sistema.".into());
    #[cfg(windows)]
    ui.set_banner("".into());

    let ui_nav = ui.as_weak();
    let host_nav = host.clone();
    let session_nav = session.clone();
    ui.on_nav_clicked(move |id| {
        if let Some(ui) = ui_nav.upgrade() {
            show_page(&ui, &session_nav, &host_nav, &id);
        }
    });

    wire_actions(&ui, &host, &session);
    show_page(&ui, &session, &host, &std::env::var("TURBO_PAGE").unwrap_or_else(|_| "home".into()));
    refresh(&ui, &host, &session, false);
    spawn_disk_sample(ui.as_weak(), host.clone(), session.clone());
    spawn_update_check(ui.as_weak(), false);
    ui.run().expect("rodar a interface");
}

fn wire_actions(ui: &MainWindow, host: &Arc<Host>, session: &Arc<Mutex<Session>>) {
    let ui_b = ui.as_weak();
    let host_b = host.clone();
    let session_b = session.clone();
    ui.on_create_backup(move || {
        if let Some(ui) = ui_b.upgrade() {
            start_backup(&ui, &host_b, &session_b, false);
        }
    });

    let ui_d = ui.as_weak();
    let host_d = host.clone();
    let session_d = session.clone();
    ui.on_diagnose(move || {
        if let Some(ui) = ui_d.upgrade() {
            refresh(&ui, &host_d, &session_d, false);
        }
    });

    let ui_m = ui.as_weak();
    let host_m = host.clone();
    let session_m = session.clone();
    ui.on_measure_disk(move || spawn_disk_sample(ui_m.clone(), host_m.clone(), session_m.clone()));

    let ui_o = ui.as_weak();
    let host_o = host.clone();
    let session_o = session.clone();
    ui.on_optimize(move |id| {
        if let Some(ui) = ui_o.upgrade() {
            request_apply(&ui, &host_o, &session_o, FollowUp::One(id.to_string()));
        }
    });

    let ui_r = ui.as_weak();
    let host_r = host.clone();
    let session_r = session.clone();
    ui.on_revert(move |id| {
        if let Some(ui) = ui_r.upgrade() {
            run_follow(&ui, &host_r, &session_r, FollowUp::RevertOne(id.to_string()));
        }
    });

    let ui_os = ui.as_weak();
    let host_os = host.clone();
    let session_os = session.clone();
    ui.on_optimize_section(move || {
        if let Some(ui) = ui_os.upgrade() {
            request_apply(&ui, &host_os, &session_os, FollowUp::Section);
        }
    });

    let ui_rs = ui.as_weak();
    let host_rs = host.clone();
    let session_rs = session.clone();
    ui.on_revert_section(move || {
        if let Some(ui) = ui_rs.upgrade() {
            run_follow(&ui, &host_rs, &session_rs, FollowUp::RevertSection);
        }
    });

    let ui_ra = ui.as_weak();
    let session_ra = session.clone();
    ui.on_revert_all(move || {
        if let Some(ui) = ui_ra.upgrade() {
            open_dialog(&ui, "Reverter tudo", "Cada item volta ao valor guardado no backup JSON. Itens que você não alterou ficam como estão.", "Reverter tudo", "Cancelar", true);
            session_ra.lock().expect("sessão").pending = Pending::Confirm(FollowUp::RevertAll);
        }
    });

    let ui_ad = ui.as_weak();
    let host_ad = host.clone();
    let session_ad = session.clone();
    ui.on_read_admin(move |_| {
        if let Some(ui) = ui_ad.upgrade() {
            read_as_admin(&ui, &host_ad, &session_ad);
        }
    });

    let ui_rm = ui.as_weak();
    let host_rm = host.clone();
    ui.on_open_resmon(move || {
        let mut message = String::new();
        with_backend(&host_rm, |sys| {
            if let Err(err) = sys.open_resource_monitor() {
                message = err.to_string();
            }
        });
        if let Some(ui) = ui_rm.upgrade() {
            if message.is_empty() {
                ui.set_toast("Monitor de Recursos aberto na aba Disco.".into());
            } else {
                ui.set_toast(message.into());
            }
        }
    });

    let ui_up = ui.as_weak();
    ui.on_check_updates(move || spawn_update_check(ui_up.clone(), true));

    let ui_yes = ui.as_weak();
    let host_yes = host.clone();
    let session_yes = session.clone();
    ui.on_accept_dialog(move || {
        if let Some(ui) = ui_yes.upgrade() {
            ui.set_dialog_open(false);
            let pending = session_yes.lock().expect("sessão").pending.clone();
            session_yes.lock().expect("sessão").pending = Pending::None;
            match pending {
                Pending::AckProtection => {
                    let next = session_yes.lock().expect("sessão").resume.take();
                    if start_backup(&ui, &host_yes, &session_yes, true) {
                        if let Some(next) = next {
                            run_follow(&ui, &host_yes, &session_yes, next);
                        }
                    }
                }
                Pending::NeedBackup(next) => {
                    session_yes.lock().expect("sessão").resume = Some(next.clone());
                    if start_backup(&ui, &host_yes, &session_yes, false) {
                        session_yes.lock().expect("sessão").resume = None;
                        run_follow(&ui, &host_yes, &session_yes, next);
                    }
                }
                Pending::Confirm(next) => run_follow(&ui, &host_yes, &session_yes, next),
                Pending::Hz { token } => {
                    session_yes.lock().expect("sessão").hz_token = token.wrapping_add(1);
                    ui.set_toast("Taxa de atualização mantida.".into());
                }
                Pending::None => {}
            }
        }
    });

    let ui_no = ui.as_weak();
    let host_no = host.clone();
    let session_no = session.clone();
    ui.on_cancel_dialog(move || {
        if let Some(ui) = ui_no.upgrade() {
            ui.set_dialog_open(false);
            let pending = session_no.lock().expect("sessão").pending.clone();
            session_no.lock().expect("sessão").pending = Pending::None;
            if let Pending::Hz { token } = pending {
                session_no.lock().expect("sessão").hz_token = token.wrapping_add(1);
                run_follow(&ui, &host_no, &session_no, FollowUp::RevertOne("g-hz".into()));
            }
        }
    });

    let ui_acc = ui.as_weak();
    ui.on_update_accept(move || {
        if let Some(ui) = ui_acc.upgrade() {
            ui.set_update_open(false);
            ui.set_busy(true);
            ui.set_busy_text("Baixando e conferindo a atualização...".into());
            let weak = ui_acc.clone();
            std::thread::spawn(move || {
                let outcome = download_and_install();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_busy(false);
                    match outcome {
                        Ok(()) => {}
                        Err(err) => {
                            ui.set_toast(err.into());
                        }
                    }
                });
            });
        }
    });

    let ui_lat = ui.as_weak();
    ui.on_update_later(move || {
        if let Some(ui) = ui_lat.upgrade() {
            ui.set_update_open(false);
        }
    });
}

fn show_page(ui: &MainWindow, session: &Mutex<Session>, host: &Host, id: &str) {
    ui.set_section_id(id.into());
    match id {
        "home" => {
            ui.set_page("home".into());
            ui.set_section_title("Início".into());
        }
        "manual" => {
            ui.set_page("table".into());
            ui.set_section_title("Checklist manual".into());
            let rows: Vec<Row3> = catalog::CHECKLIST.iter().map(|row| Row3 { a: row.item.into(), b: row.onde.into(), c: row.fonte.into() }).collect();
            ui.set_rows(model(rows));
        }
        "limites" => {
            ui.set_page("table".into());
            ui.set_section_title("O app não faz isso".into());
            let rows: Vec<Row3> = catalog::NAO_FAZ.iter().map(|row| Row3 { a: row.coisa.into(), b: row.porque.into(), c: "".into() }).collect();
            ui.set_rows(model(rows));
        }
        "log" => {
            ui.set_page("log".into());
            ui.set_section_title("Log de alterações".into());
            ui.set_log_text(read_log_text().into());
        }
        "sobre" => {
            ui.set_page("about".into());
            ui.set_section_title("Sobre".into());
        }
        other => {
            ui.set_page("section".into());
            let title = Section::all().iter().find(|s| s.id() == other).map(|s| s.label()).unwrap_or(other);
            ui.set_section_title(title.into());
            let views = session.lock().expect("sessão").views.clone();
            let cards: Vec<Card> = views.into_iter().filter(|v| v.section.id() == other).map(card_from).collect();
            ui.set_cards(model(cards));
            let _ = host;
        }
    }
    fill_nav(ui, session);
}

fn refresh(ui: &MainWindow, host: &Host, session: &Mutex<Session>, keep_page: bool) {
    let (backup, disk, admin) = {
        let guard = session.lock().expect("sessão");
        (guard.backup.clone(), guard.disk.clone(), guard.admin.clone())
    };
    let mut views = Vec::new();
    let mut error = None;
    with_backend(host, |sys| {
        let ctx = ReadCache { disk: disk.as_ref(), disks: admin.disks.as_deref(), security: admin.security.as_ref() };
        match engine::diagnose_all(sys, &ctx, backup.as_ref()) {
            Ok(found) => views = found,
            Err(err) => error = Some(err.to_string()),
        }
    });
    if let Some(err) = error {
        ui.set_toast(err.into());
        return;
    }
    let ok = views.iter().filter(|v| v.state == ItemState::Ok).count();
    let improve = views.iter().filter(|v| v.state == ItemState::CanImprove).count();
    let unread = views.iter().filter(|v| matches!(v.state, ItemState::NotRead | ItemState::NeedsAdmin)).count();
    ui.set_ok_count(ok.to_string().into());
    ui.set_improve_count(improve.to_string().into());
    ui.set_unread_count(unread.to_string().into());
    ui.set_critical(critical_text(&views).into());
    ui.set_backup_text(backup_text(backup.as_ref()).into());
    {
        let mut guard = session.lock().expect("sessão");
        guard.views = views;
    }
    let page = if keep_page { ui.get_section_id().to_string() } else { ui.get_section_id().to_string() };
    show_page(ui, session, host, &page);
}

fn start_backup(ui: &MainWindow, host: &Host, session: &Mutex<Session>, ack: bool) -> bool {
    let mut created = None;
    let mut denied = None;
    let mut failed = None;
    with_backend(host, |sys| match engine::create_backup(sys, ack) {
        Ok(backup) => created = Some(backup),
        Err(turbo::model::EngineError::ProtectionDisabled { message }) => denied = Some(message),
        Err(err) => failed = Some(err.to_string()),
    });
    if let Some(message) = denied {
        open_dialog(ui, "Proteção do Sistema desligada", &message, "Continuar só com o JSON", "Agora não", true);
        session.lock().expect("sessão").pending = Pending::AckProtection;
        return false;
    }
    if let Some(err) = failed {
        ui.set_toast(err.into());
        return false;
    }
    let backup = created.expect("backup");
    if store::save_backup(&store::backup_path(), &backup).is_err() {
        ui.set_toast("Não consegui gravar o backup JSON.".into());
        return false;
    }
    let _ = store::append_log(&store::log_path(), &engine::backup_log_entry(&backup));
    ui.set_toast(backup.restore_point.message.clone().into());
    session.lock().expect("sessão").backup = Some(backup);
    refresh(ui, host, session, true);
    true
}

fn request_apply(ui: &MainWindow, host: &Arc<Host>, session: &Arc<Mutex<Session>>, follow: FollowUp) {
    let has_backup = session.lock().expect("sessão").backup.is_some();
    if !has_backup {
        open_dialog(ui, "Backup obrigatório", "Antes da primeira alteração o Turbo cria um ponto de restauração, se a Proteção do Sistema permitir, e guarda os valores originais num JSON.", "Criar backup e continuar", "Cancelar", true);
        session.lock().expect("sessão").pending = Pending::NeedBackup(follow);
        return;
    }
    let body = match &follow {
        FollowUp::One(id) => {
            let caveat = meta(id).and_then(|m| m.caveat).unwrap_or("");
            if caveat.is_empty() {
                run_follow(ui, host, session, follow);
                return;
            }
            format!("{caveat}\n\nAplicar mesmo assim?")
        }
        FollowUp::Section => "Otimizar os itens desta seção que podem melhorar. Itens só de leitura e os condicionais (SysMain e Spooler) ficam de fora.".into(),
        _ => String::new(),
    };
    let title = match &follow {
        FollowUp::One(id) => meta(id).map(|m| m.name).unwrap_or("Otimizar"),
        _ => "Otimizar a seção",
    };
    open_dialog(ui, title, &body, "Otimizar", "Cancelar", true);
    session.lock().expect("sessão").pending = Pending::Confirm(follow);
}

fn run_follow(ui: &MainWindow, host: &Arc<Host>, session: &Arc<Mutex<Session>>, follow: FollowUp) {
    let backup = session.lock().expect("sessão").backup.clone();
    let Some(backup) = backup else {
        ui.set_toast("Crie o backup antes de alterar.".into());
        return;
    };
    let section_id = ui.get_section_id().to_string();
    let section = Section::all().iter().copied().find(|s| s.id() == section_id);
    let disk = session.lock().expect("sessão").disk.clone();
    let admin = session.lock().expect("sessão").admin.clone();
    let mut local = Vec::new();
    let mut admin_ids = Vec::new();
    let mut note = None;
    with_backend(host, |sys| {
        let ctx = ReadCache { disk: disk.as_ref(), disks: admin.disks.as_deref(), security: admin.security.as_ref() };
        let plan = match &follow {
            FollowUp::One(id) | FollowUp::RevertOne(id) => {
                let needs = turbo::tweaks::requires_admin(id, sys).unwrap_or(false) && !sys.is_elevated();
                if needs {
                    admin_ids.push(id.clone());
                } else {
                    local.push(id.clone());
                }
                return;
            }
            FollowUp::Section => engine::plan_optimize(sys, &ctx, section),
            FollowUp::RevertSection => engine::plan_revert(sys, &ctx, Some(&backup), section),
            FollowUp::RevertAll => engine::plan_revert(sys, &ctx, Some(&backup), None),
        };
        match plan {
            Ok(plan) => {
                local = plan.local;
                admin_ids = plan.needs_admin;
            }
            Err(err) => note = Some(err.to_string()),
        }
    });
    if let Some(err) = note {
        ui.set_toast(err.into());
        return;
    }
    if local.is_empty() && admin_ids.is_empty() {
        ui.set_toast("Nada para fazer aqui.".into());
        return;
    }
    let reverting = matches!(follow, FollowUp::RevertOne(_) | FollowUp::RevertSection | FollowUp::RevertAll);
    let log = store::log_path();
    let mut lines = Vec::new();
    with_backend(host, |sys| {
        let ctx = ReadCache { disk: disk.as_ref(), disks: admin.disks.as_deref(), security: admin.security.as_ref() };
        let results = if reverting {
            engine::revert_ids(sys, &backup, &local, Some(&log))
        } else {
            engine::apply_ids(sys, &ctx, &backup, &local, Some(&log))
        };
        for result in results {
            lines.push(match result {
                Ok(ok) => ok.message,
                Err(err) => err,
            });
        }
    });
    if local.iter().any(|id| id == "g-hz") && !reverting && lines.iter().any(|l| !l.contains("não confirmou") && !l.contains("restaurou")) {
        arm_hz_confirm(ui, host, session);
    }
    if !admin_ids.is_empty() {
        match elevate_ids(&admin_ids, reverting) {
            Ok(extra) => lines.extend(extra),
            Err(err) => lines.push(err),
        }
    }
    ui.set_toast(lines.join(" ").into());
    refresh(ui, host, session, true);
    ui.set_log_text(read_log_text().into());
}

fn arm_hz_confirm(ui: &MainWindow, host: &Arc<Host>, session: &Arc<Mutex<Session>>) {
    let token = {
        let mut guard = session.lock().expect("sessão");
        guard.hz_token = guard.hz_token.wrapping_add(1);
        guard.hz_token
    };
    open_dialog(ui, "A imagem ficou estável?", "Se você não confirmar em 15 segundos, o Turbo volta a taxa anterior.", "Ficou boa", "Reverter agora", true);
    session.lock().expect("sessão").pending = Pending::Hz { token };
    let weak = ui.as_weak();
    let host = host.clone();
    let session = session.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(15));
        let still = session.lock().expect("sessão").hz_token == token;
        if !still {
            return;
        }
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let current = session.lock().expect("sessão").hz_token;
            if current != token {
                return;
            }
            session.lock().expect("sessão").pending = Pending::None;
            ui.set_dialog_open(false);
            run_follow(&ui, &host, &session, FollowUp::RevertOne("g-hz".into()));
            ui.set_toast("15 segundos sem confirmação. Taxa anterior restaurada.".into());
        });
    });
}

fn read_as_admin(ui: &MainWindow, host: &Host, session: &Mutex<Session>) {
    #[cfg(not(windows))]
    {
        if let Host::Demo(fake) = host {
            let mut fake = fake.lock().expect("demo");
            fake.elevated = true;
            fake.deny_admin_reads = false;
        }
        ui.set_toast("Leitura de administrador simulada.".into());
        refresh(ui, host, session, true);
    }
    #[cfg(windows)]
    {
        let result = store::data_dir().join("admin-cache.json");
        let args = format!("--read-admin --result \"{}\"", result.display());
        if !elevate_command(&args) {
            ui.set_toast("Elevação cancelada. Nada foi lido como admin.".into());
            return;
        }
        if wait_file(&result, Duration::from_secs(40)) {
            session.lock().expect("sessão").admin = store::load_admin_cache(&result);
            refresh(ui, host, session, true);
            ui.set_toast("Leitura de administrador concluída.".into());
        } else {
            ui.set_toast("A leitura elevada não devolveu resultado.".into());
        }
    }
}

fn elevate_ids(ids: &[String], revert: bool) -> Result<Vec<String>, String> {
    #[cfg(not(windows))]
    {
        let _ = (ids, revert);
        Err("Itens de administrador só aplicam no Windows, com UAC.".into())
    }
    #[cfg(windows)]
    {
        let batch_path = store::data_dir().join("pending-apply.json");
        let result_path = store::data_dir().join("apply-result.json");
        let _ = std::fs::remove_file(&result_path);
        let payload = serde_json::json!({ "ids": ids, "revert": revert });
        std::fs::write(&batch_path, serde_json::to_vec(&payload).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let args = format!("--apply \"{}\" --result \"{}\"", batch_path.display(), result_path.display());
        if !elevate_command(&args) {
            return Err("Elevação cancelada. Nada que pede admin foi alterado.".into());
        }
        if !wait_file(&result_path, Duration::from_secs(60)) {
            return Err("O processo elevado não devolveu resultado.".into());
        }
        let bytes = std::fs::read(&result_path).map_err(|e| e.to_string())?;
        let lines: Vec<String> = serde_json::from_slice(&bytes).unwrap_or_else(|_| vec!["resultado elevado ilegível".into()]);
        Ok(lines)
    }
}

fn spawn_disk_sample(ui: slint::Weak<MainWindow>, host: Arc<Host>, session: Arc<Mutex<Session>>) {
    if let Some(ui) = ui.upgrade() {
        ui.set_busy(true);
        ui.set_busy_text("Medindo o disco por 10 segundos. Nada será alterado.".into());
    }
    std::thread::spawn(move || {
        let mut report = None;
        let mut error = None;
        with_backend(&host, |sys| match sys.sample_disk_usage() {
            Ok(found) => report = Some(found),
            Err(err) => error = Some(err.to_string()),
        });
        let _ = ui.upgrade_in_event_loop(move |ui| {
            ui.set_busy(false);
            if let Some(err) = error {
                ui.set_toast(err.into());
                return;
            }
            if let Some(report) = report {
                session.lock().expect("sessão").disk = Some(report);
            }
            refresh(&ui, &host, &session, true);
        });
    });
}

fn spawn_update_check(ui: slint::Weak<MainWindow>, manual: bool) {
    std::thread::spawn(move || {
        let result = check_update();
        let _ = ui.upgrade_in_event_loop(move |ui| match result {
            Ok(Some(manifest)) => {
                let title = format!("Versão {} disponível", manifest.version);
                let body = manifest.notes.clone();
                if let Ok(text) = serde_json::to_string(&ManifestCache::from(manifest)) {
                    let _ = std::fs::write(store::data_dir().join("pending-update.json"), text);
                }
                ui.set_update_title(title.into());
                ui.set_update_body(body.into());
                ui.set_update_open(true);
            }
            Ok(None) if manual => ui.set_toast("Você já está na versão mais recente.".into()),
            Err(err) if manual => ui.set_toast(err.into()),
            _ => {}
        });
    });
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ManifestCache {
    version: String,
    notes: String,
    asset_name: String,
    asset_url: String,
    sha256: String,
    signature: Option<String>,
}

impl From<UpdateManifest> for ManifestCache {
    fn from(m: UpdateManifest) -> Self {
        Self { version: m.version, notes: m.notes, asset_name: m.asset_name, asset_url: m.asset_url, sha256: m.sha256, signature: m.signature }
    }
}

impl From<ManifestCache> for UpdateManifest {
    fn from(m: ManifestCache) -> Self {
        Self { version: m.version, notes: m.notes, asset_name: m.asset_name, asset_url: m.asset_url, sha256: m.sha256, signature: m.signature, checksum_url: None }
    }
}

fn check_update() -> Result<Option<UpdateManifest>, String> {
    let url = update_url();
    let manifest = updater::load_manifest(&url).map_err(|e| e.to_string())?;
    if updater::decide(APP_VERSION, &manifest.version) == UpdateDecision::Available {
        Ok(Some(manifest))
    } else {
        Ok(None)
    }
}

fn update_url() -> String {
    let env_url = std::env::var("TURBO_UPDATE_MANIFEST_URL").ok();
    let beside_exe = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| updater::read_update_source(&dir.join("update-source.url")))).flatten();
    updater::resolve_update_url(env_url.as_deref(), &[updater::read_update_source(&store::update_source_path()), beside_exe])
}

fn download_and_install() -> Result<(), String> {
    let path = store::data_dir().join("pending-update.json");
    let bytes = std::fs::read(&path).map_err(|_| "Não achei os dados da atualização.".to_string())?;
    let cached: ManifestCache = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let manifest = UpdateManifest::from(cached);
    let file = updater::fetch_bytes(&manifest.asset_url).map_err(|e| e.to_string())?;
    updater::verify_download(&file, &manifest.sha256, manifest.signature.as_deref(), Some(updater::embedded_public_key())).map_err(|e| e.to_string())?;
    let dir = store::data_dir().join("updates");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let setup = dir.join("TurboApp-Setup.exe");
    std::fs::write(&setup, &file).map_err(|e| e.to_string())?;
    launch_detached(&setup)?;
    std::process::exit(0);
}

fn launch_detached(setup: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let pid = std::process::id();
    std::process::Command::new(exe)
        .arg("--launch-installer")
        .arg(setup)
        .arg(pid.to_string())
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn wait_and_launch(setup: &Path, pid: u32) {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
        unsafe {
            if let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                let _ = WaitForSingleObject(handle, 20_000);
                let _ = CloseHandle(handle);
            }
        }
        let _ = WAIT_OBJECT_0;
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        std::thread::sleep(Duration::from_millis(300));
    }
    std::thread::sleep(Duration::from_millis(700));
    let _ = std::process::Command::new(setup).spawn();
}

fn run_apply_batch(batch_path: &Path, result_path: Option<&str>) -> i32 {
    let Ok(bytes) = std::fs::read(batch_path) else { return 2 };
    let payload: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    let ids: Vec<String> = payload.get("ids").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
    let revert = payload.get("revert").and_then(|v| v.as_bool()).unwrap_or(false);
    let Ok(Some(backup)) = store::load_backup(&store::backup_path()) else {
        write_result(result_path, vec!["Backup não encontrado.".into()]);
        return 3;
    };
    let mut lines = Vec::new();
    let ran = with_backend_result(|sys| {
        let ctx = ReadCache::empty();
        let results: Vec<Result<ApplyOutcome, String>> = if revert {
            engine::revert_ids(sys, &backup, &ids, Some(&store::log_path()))
        } else {
            engine::apply_ids(sys, &ctx, &backup, &ids, Some(&store::log_path()))
        };
        for result in results {
            lines.push(match result {
                Ok(ok) => ok.message,
                Err(err) => err,
            });
        }
    });
    if let Err(err) = ran {
        lines.push(err);
    }
    write_result(result_path, lines);
    0
}

fn run_admin_read(result: &Path) -> i32 {
    let mut cache = AdminCacheFile::default();
    let ran = with_backend_result(|sys| {
        cache.disks = sys.disks().ok();
        cache.security = sys.security_status().ok();
    });
    if let Err(err) = ran {
        let _ = std::fs::write(result, format!("{{\"error\":\"{err}\"}}"));
        return 4;
    }
    let _ = store::save_admin_cache(result, &cache);
    0
}

fn write_result(path: Option<&str>, lines: Vec<String>) {
    let Some(path) = path else { return };
    let _ = std::fs::write(path, serde_json::to_vec(&lines).unwrap_or_default());
}

fn with_backend(host: &Host, f: impl FnOnce(&mut dyn SystemBackend)) {
    let _ = with_backend_result_on(host, |sys| {
        f(sys);
        Ok::<(), String>(())
    });
}

fn with_backend_result(f: impl FnOnce(&mut dyn SystemBackend)) -> Result<(), String> {
    #[cfg(windows)]
    {
        let mut backend = turbo::platform::WindowsBackend::new()?;
        f(&mut backend);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = f;
        Err("este comando só existe no Windows".into())
    }
}

fn with_backend_result_on(host: &Host, f: impl FnOnce(&mut dyn SystemBackend) -> Result<(), String>) -> Result<(), String> {
    match host {
        Host::Demo(fake) => f(&mut *fake.lock().expect("demo")),
        #[cfg(windows)]
        Host::Windows => {
            let mut backend = turbo::platform::WindowsBackend::new()?;
            f(&mut backend)
        }
    }
}

#[cfg(windows)]
fn elevate_command(args: &str) -> bool {
    use windows::core::{w, HSTRING};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let Ok(exe) = std::env::current_exe() else { return false };
    let exe = HSTRING::from(exe.as_os_str());
    let params = HSTRING::from(args);
    let code = unsafe { ShellExecuteW(None, w!("runas"), &exe, &params, None, SW_SHOWNORMAL) };
    code.0 as isize > 32
}

#[cfg(windows)]
fn wait_file(path: &Path, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if path.exists() && std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false) {
            std::thread::sleep(Duration::from_millis(200));
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

fn card_from(view: ItemView) -> Card {
    let kind = match view.state {
        ItemState::Ok => "ok",
        ItemState::CanImprove => "improve",
        ItemState::NotRead | ItemState::NeedsAdmin => "unread",
    };
    Card {
        id: view.id.clone().into(),
        name: view.name.into(),
        faz: view.faz.into(),
        ajuda: view.ajuda.into(),
        detail: view.detail.into(),
        seal: view.seal.label().into(),
        caveat: view.caveat.into(),
        admin: view.admin.into(),
        reboot: view.reinicio.into(),
        state: view.state.label().into(),
        kind: kind.into(),
        can_optimize: view.can_optimize,
        can_revert: view.can_revert,
        readonly: view.readonly,
        needs_admin_read: view.needs_admin_read,
        show_resmon: view.id == "d-uso",
    }
}

fn fill_nav(ui: &MainWindow, session: &Mutex<Session>) {
    let views = session.lock().expect("sessão").views.clone();
    let mut entries = vec![NavEntry { id: "home".into(), label: "Início".into(), hint: "".into() }];
    for section in Section::all() {
        let improve = views.iter().filter(|v| v.section == *section && v.state == ItemState::CanImprove).count();
        entries.push(NavEntry {
            id: section.id().into(),
            label: section.label().into(),
            hint: if improve == 0 { "".into() } else { improve.to_string().into() },
        });
    }
    entries.push(NavEntry { id: "manual".into(), label: "Checklist manual".into(), hint: "".into() });
    entries.push(NavEntry { id: "limites".into(), label: "O que não fazemos".into(), hint: "".into() });
    entries.push(NavEntry { id: "log".into(), label: "Log".into(), hint: "".into() });
    entries.push(NavEntry { id: "sobre".into(), label: "Sobre".into(), hint: "".into() });
    ui.set_nav(model(entries));
}

fn critical_text(views: &[ItemView]) -> String {
    let state = |id: &str| views.iter().find(|v| v.id == id).map(|v| v.state.label()).unwrap_or("—");
    [
        format!("Saúde e uso do disco — {} / {} / {}", state("d-smart"), state("d-uso"), state("d-onde")),
        format!("Aceleração do mouse — {}", state("m-acel")),
        format!("Apps na inicialização e Edge — {} / {}", state("i-run"), state("i-edge")),
        format!("Gravação em segundo plano — {}", state("p-dvr")),
        format!("Atalho das Teclas de Aderência — {}", state("k-atalhos")),
    ]
    .join("\n")
}

fn backup_text(backup: Option<&BackupFile>) -> String {
    match backup {
        None => "Nenhum backup ainda. O botão cria o ponto de restauração (ou explica se a Proteção do Sistema estiver desligada) e salva os valores originais.".into(),
        Some(backup) => format!("Backup de {} · {}", backup.created_at, backup.restore_point.message),
    }
}

fn read_log_text() -> String {
    match store::read_log(&store::log_path()) {
        Ok(entries) if entries.is_empty() => String::new(),
        Ok(entries) => entries.into_iter().rev().take(80).rev().map(|e| format!("{}  {}  {}  {}", e.ts, e.action, e.id, e.message)).collect::<Vec<_>>().join("\n"),
        Err(err) => err,
    }
}

fn open_dialog(ui: &MainWindow, title: &str, body: &str, primary: &str, secondary: &str, secondary_visible: bool) {
    ui.set_dialog_title(title.into());
    ui.set_dialog_body(body.into());
    ui.set_dialog_primary(primary.into());
    ui.set_dialog_secondary(secondary.into());
    ui.set_dialog_secondary_visible(secondary_visible);
    ui.set_dialog_open(true);
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}
