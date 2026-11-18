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
    EnableProtection,
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
    install_panic_hook();
    store::trace(&format!("processo Turbo {}", turbo::model::APP_VERSION));
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
    turbo::platform::mark_ui_thread();
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
    let weak = ui.as_weak();
    let host_boot = host.clone();
    let session_boot = session.clone();
    // A janela do winit precisa de COM STA. O diagnóstico só começa depois
    // que o loop está no ar, e o COM MTA fica numa thread de trabalho.
    slint::Timer::single_shot(Duration::from_millis(0), move || {
        if let Some(ui) = weak.upgrade() {
            let mut elevated = true;
            with_backend(&host_boot, |sys| elevated = sys.is_elevated());
            // No Linux a demonstração não é um processo elevado, mas os botões
            // do exemplo precisam continuar usáveis. No Windows o manifesto
            // pede administrador; se mesmo assim não estiver elevado, a faixa aparece.
            #[cfg(windows)]
            ui.set_elevated(elevated);
            #[cfg(not(windows))]
            ui.set_elevated(true);
            store::trace(&format!("janela aberta elevado={elevated}"));
            refresh(&ui, &host_boot, &session_boot, false);
            spawn_disk_sample(ui.as_weak(), host_boot.clone(), session_boot.clone());
            spawn_update_check(ui.as_weak(), false);
        }
    });
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

    let ui_admin = ui.as_weak();
    ui.on_relaunch_admin(move || {
        #[cfg(windows)]
        {
            if elevate_command("") {
                std::process::exit(0);
            }
            if let Some(ui) = ui_admin.upgrade() {
                ui.set_toast("O Windows não abriu a janela de administrador.".into());
            }
        }
        #[cfg(not(windows))]
        {
            if let Some(ui) = ui_admin.upgrade() {
                ui.set_toast("Reabrir como administrador só existe no Windows.".into());
            }
        }
    });

    let ui_yes = ui.as_weak();
    let host_yes = host.clone();
    let session_yes = session.clone();
    ui.on_accept_dialog(move || {
        if let Some(ui) = ui_yes.upgrade() {
            ui.set_dialog_open(false);
            let pending = session_yes.lock().expect("sessão").pending.clone();
            session_yes.lock().expect("sessão").pending = Pending::None;
            match pending {
                Pending::EnableProtection => {
                    let next = session_yes.lock().expect("sessão").resume.take();
                    if enable_protection_and_backup(&ui, &host_yes, &session_yes) {
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
            match pending {
                Pending::Hz { token } => {
                    session_no.lock().expect("sessão").hz_token = token.wrapping_add(1);
                    run_follow(&ui, &host_no, &session_no, FollowUp::RevertOne("g-hz".into()));
                }
                Pending::EnableProtection => {
                    let next = session_no.lock().expect("sessão").resume.take();
                    if start_backup(&ui, &host_no, &session_no, true) {
                        if let Some(next) = next {
                            run_follow(&ui, &host_no, &session_no, next);
                        }
                    }
                }
                _ => {}
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
            let elevated = ui.get_elevated();
            let cards: Vec<Card> = views.into_iter().filter(|v| v.section.id() == other).map(|view| card_from(view, elevated)).collect();
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
        let message = format!("O diagnóstico inteiro falhou: {err}");
        store::trace(&message);
        ui.set_toast(message.into());
        return;
    }
    let failed: Vec<String> = views.iter().filter(|v| v.detail.starts_with("Falha ao ler")).map(|v| v.detail.clone()).collect();
    if failed.is_empty() {
        let previous = ui.get_toast().to_string();
        if previous.contains("0x") || previous.contains("HRESULT") || previous.starts_with("Falha ao ler") || previous.starts_with("O diagnóstico") {
            ui.set_toast("".into());
        }
    } else {
        ui.set_toast(failed.join(" · ").into());
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
        store::trace("ação backup proteção desligada, pedindo confirmação");
        open_dialog(
            ui,
            "Proteção do Sistema desligada",
            &format!("{message}\n\nPosso ligar a proteção no disco do Windows para criar o ponto. Reverter tudo desliga de novo, se o Turbo tiver ligado."),
            "Ativar proteção e criar o ponto",
            "Continuar só com o JSON",
            true,
        );
        session.lock().expect("sessão").pending = Pending::EnableProtection;
        return false;
    }
    if let Some(err) = failed {
        store::trace(&format!("ação backup falhou: {err}"));
        ui.set_toast(err.clone().into());
        open_dialog(ui, "Não criei o backup", &err, "Ok", "", false);
        return false;
    }
    let Some(backup) = created else {
        let message = "O backup não devolveu resultado. Veja %LOCALAPPDATA%\\TurboApp\\turbo.log.".to_string();
        store::trace(&format!("ação backup sem resultado"));
        ui.set_toast(message.clone().into());
        open_dialog(ui, "Não criei o backup", &message, "Ok", "", false);
        return false;
    };
    if store::save_backup(&store::backup_path(), &backup).is_err() {
        let message = "Não consegui gravar o backup JSON em %LOCALAPPDATA%\\TurboApp\\backup.json.".to_string();
        store::trace(&format!("ação backup {message}"));
        ui.set_toast(message.clone().into());
        open_dialog(ui, "Não gravei o backup", &message, "Ok", "", false);
        return false;
    }
    let _ = store::append_log(&store::log_path(), &engine::backup_log_entry(&backup));
    let title = match backup.restore_point.status {
        turbo::model::RestorePointStatus::Created => "Backup criado",
        turbo::model::RestorePointStatus::UsedExisting => "Backup criado com o ponto que já existia",
        turbo::model::RestorePointStatus::ProtectionDisabled => "Backup só com o JSON",
        turbo::model::RestorePointStatus::Failed => "Backup incompleto",
    };
    let message = backup.restore_point.message.clone();
    store::trace(&format!("ação backup gravado: {message}"));
    ui.set_toast(message.clone().into());
    open_dialog(ui, title, &message, "Ok", "", false);
    session.lock().expect("sessão").pending = Pending::None;
    session.lock().expect("sessão").backup = Some(backup);
    refresh(ui, host, session, true);
    true
}

fn enable_protection_and_backup(ui: &MainWindow, host: &Host, session: &Mutex<Session>) -> bool {
    let mut error = None;
    with_backend(host, |sys| {
        if let Err(err) = sys.set_system_protection(true) {
            error = Some(err.to_string());
        }
    });
    if let Some(err) = error {
        let message = format!("Não liguei a Proteção do Sistema: {err}");
        store::trace(&format!("ação {message}"));
        ui.set_toast(message.clone().into());
        open_dialog(ui, "Proteção do Sistema", &message, "Ok", "", false);
        return false;
    }
    if !start_backup(ui, host, session, false) {
        return false;
    }
    let mut guard = session.lock().expect("sessão");
    if let Some(backup) = guard.backup.as_mut() {
        backup.restore_point.protection_enabled_by_app = true;
        let _ = store::save_backup(&store::backup_path(), backup);
        store::trace("ação proteção do sistema ligada pelo Turbo e marcada para reversão");
    }
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
    let reverting = matches!(follow, FollowUp::RevertOne(_) | FollowUp::RevertSection | FollowUp::RevertAll);
    let turn_protection_off = matches!(follow, FollowUp::RevertAll) && backup.restore_point.protection_enabled_by_app;
    if local.is_empty() && admin_ids.is_empty() && !turn_protection_off {
        ui.set_toast("Nada para fazer aqui.".into());
        return;
    }
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
        if turn_protection_off {
            match sys.set_system_protection(false) {
                Ok(()) => lines.push("Proteção do Sistema desligada de novo, como estava antes do Turbo.".into()),
                Err(err) => lines.push(format!("Não consegui desligar a Proteção do Sistema que o Turbo tinha ligado: {err}")),
            }
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

fn with_backend(host: &Host, f: impl FnOnce(&mut dyn SystemBackend) + Send) {
    let _ = with_backend_result_on(host, |sys| {
        f(sys);
        Ok::<(), String>(())
    });
}

fn with_backend_result(f: impl FnOnce(&mut dyn SystemBackend) + Send) -> Result<(), String> {
    #[cfg(windows)]
    {
        on_com_thread(|sys| {
            f(sys);
            Ok(())
        })
    }
    #[cfg(not(windows))]
    {
        let _ = f;
        Err("este comando só existe no Windows".into())
    }
}

fn with_backend_result_on(host: &Host, f: impl FnOnce(&mut dyn SystemBackend) -> Result<(), String> + Send) -> Result<(), String> {
    match host {
        Host::Demo(fake) => f(&mut *fake.lock().expect("demo")),
        #[cfg(windows)]
        Host::Windows => on_com_thread(f),
    }
}

/// COM MTA numa thread que não é a da janela. O winit chama OleInitialize (STA)
/// na thread principal; misturar os dois no mesmo thread derruba o processo
/// com RPC_E_CHANGED_MODE (0xc0000409).
#[cfg(windows)]
fn on_com_thread(f: impl FnOnce(&mut dyn SystemBackend) -> Result<(), String> + Send) -> Result<(), String> {
    let mut out = None;
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new().name("turbo-com".into()).spawn_scoped(scope, || {
            turbo::platform::WindowsBackend::with_session(|backend| f(backend))
        });
        out = Some(match handle {
            Ok(handle) => match handle.join() {
                Ok(Ok(inner)) => inner,
                Ok(Err(err)) => Err(err),
                Err(_) => Err("a thread COM encerrou com panic".into()),
            },
            Err(err) => Err(err.to_string()),
        });
    });
    out.unwrap_or_else(|| Err("a thread COM não devolveu resultado".into()))
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let message = format!("{info}\n\n{backtrace}");
        let path = store::data_dir().join("crash.log");
        let _ = std::fs::create_dir_all(store::data_dir());
        let entry = format!("\n---- {} ----\n{message}\n", turbo::model::now_rfc3339());
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&path).and_then(|mut file| {
            use std::io::Write;
            file.write_all(entry.as_bytes())
        });
        #[cfg(windows)]
        show_crash_box(&message, &path);
        eprintln!("{message}");
    }));
}

#[cfg(windows)]
fn show_crash_box(message: &str, log_path: &Path) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND};
    let short: String = message.chars().take(1200).collect();
    let text = format!("O Turbo encontrou um erro e precisa fechar.\n\n{short}\n\nDetalhes em {}", log_path.display());
    let text = HSTRING::from(text);
    let title = HSTRING::from("Turbo App");
    unsafe {
        MessageBoxW(None, &text, &title, MB_OK | MB_ICONERROR | MB_SETFOREGROUND);
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

fn card_from(view: ItemView, elevated: bool) -> Card {
    let admin_locked = !elevated && meta(&view.id).map(|item| item.needs_admin_apply).unwrap_or(false);
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
        admin_locked,
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
