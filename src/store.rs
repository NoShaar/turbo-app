//! Backup JSON e log local. Os dois ficam em `%LOCALAPPDATA%\TurboApp` e
//! sobrevivem a atualização e desinstalação do executável.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{now_rfc3339, BackupFile};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub ts: String,
    pub id: String,
    pub action: String,
    pub ok: bool,
    pub message: String,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
}

pub fn save_backup(path: &Path, backup: &BackupFile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_vec_pretty(backup).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load_backup(path: &Path) -> Result<Option<BackupFile>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let backup = serde_json::from_slice(&bytes).map_err(|e| format!("backup ilegível: {e}"))?;
    Ok(Some(backup))
}

pub fn append_log(path: &Path, entry: &LogEntry) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut line = serde_json::to_string(entry).map_err(|e| e.to_string())?;
    line.push('\n');
    let mut file = OpenOptions::new().create(true).append(true).open(path).map_err(|e| e.to_string())?;
    file.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn read_log(path: &Path) -> Result<Vec<LogEntry>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry = serde_json::from_str(line).map_err(|e| format!("linha {} do log: {e}", idx + 1))?;
        out.push(entry);
    }
    Ok(out)
}

pub fn log_change(path: &Path, id: &str, action: &str, ok: bool, message: &str, before: &impl Serialize, after: &impl Serialize) -> Result<(), String> {
    let entry = LogEntry {
        ts: now_rfc3339(),
        id: id.to_string(),
        action: action.to_string(),
        ok,
        message: message.to_string(),
        before: serde_json::to_value(before).unwrap_or(serde_json::Value::Null),
        after: serde_json::to_value(after).unwrap_or(serde_json::Value::Null),
    };
    append_log(path, &entry)
}

pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TURBO_DATA_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(local).join("TurboApp");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".local").join("share").join("TurboApp")
}

pub fn backup_path() -> PathBuf {
    data_dir().join("backup.json")
}

pub fn log_path() -> PathBuf {
    data_dir().join("log.jsonl")
}

pub fn admin_cache_path() -> PathBuf {
    data_dir().join("admin-cache.json")
}

pub fn update_source_path() -> PathBuf {
    data_dir().join("update-source.url")
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AdminCacheFile {
    pub disks: Option<Vec<crate::model::DiskHealth>>,
    pub security: Option<crate::model::SecurityStatus>,
}

pub fn load_admin_cache(path: &Path) -> AdminCacheFile {
    let Ok(bytes) = fs::read(path) else { return AdminCacheFile::default(); };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn save_admin_cache(path: &Path, cache: &AdminCacheFile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(cache).map_err(|e| e.to_string())?;
    fs::write(path, bytes).map_err(|e| e.to_string())
}
