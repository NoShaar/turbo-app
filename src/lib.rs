//! Motor do Turbo: diagnóstico, backup e reversão sem interface.
//! A UI e o backend do Windows ficam por cima desta API.

pub mod backend;
pub mod catalog;
pub mod engine;
pub mod fake;
pub mod model;
pub mod platform;
pub mod store;
pub mod tweaks;
pub mod updater;

pub use engine::{apply_ids, apply_one, create_backup, diagnose_all, plan_optimize, plan_revert, revert_ids, revert_one};
pub use model::{BackupFile, ItemView, Section};
pub use updater::{decide, load_manifest, verify_download};
