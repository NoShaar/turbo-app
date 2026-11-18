//! Verificação de atualização. Não há serviço residente: a checagem roda
//! uma vez ao abrir o app e quando a pessoa pede.
//!
//! O download só é executado depois que o SHA-256 bate e, com a chave
//! pública embutida, a assinatura Ed25519 confere.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::model::APP_VERSION;

pub const EMBEDDED_PUBLIC_KEY_HEX: &str = include_str!("../releases/update-public-key.txt");

/// Manifesto público da release mais recente. Não precisa de token.
pub const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/NoShaar/turbo-app/releases/latest/download/latest.json";

/// URL direta do instalador da versão 1, gravada no `latest.json` publicado.
pub const V1_SETUP_URL: &str =
    "https://github.com/NoShaar/turbo-app/releases/download/v1.0.0/TurboApp-Setup.exe";

const SETUP_NAME: &str = "TurboApp-Setup.exe";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateManifest {
    pub version: String,
    pub notes: String,
    pub asset_name: String,
    pub asset_url: String,
    pub sha256: String,
    pub signature: Option<String>,
    pub checksum_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateDecision {
    UpToDate,
    Available,
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("não consegui consultar atualizações: {0}")]
    Network(String),
    #[error("manifesto de atualização inválido: {0}")]
    Manifest(String),
    #[error("o arquivo baixado não confere com o SHA-256 publicado")]
    HashMismatch,
    #[error("a assinatura do arquivo não confere com a chave do Turbo")]
    BadSignature,
    #[error("atualização sem assinatura. O Turbo não instala arquivo sem assinatura válida.")]
    MissingSignature,
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Deserialize)]
struct PlainManifest {
    version: String,
    notes: String,
    setup: PlainSetup,
}

#[derive(Debug, Deserialize)]
struct PlainSetup {
    name: String,
    url: String,
    sha256: String,
    #[serde(default)]
    signature: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

pub fn parse_plain_manifest(text: &str) -> Result<UpdateManifest, UpdateError> {
    let parsed: PlainManifest = serde_json::from_str(text).map_err(|e| UpdateError::Manifest(e.to_string()))?;
    if parsed.version.trim().is_empty() || parsed.setup.url.trim().is_empty() || parsed.setup.sha256.trim().is_empty() {
        return Err(UpdateError::Manifest("versão, url e sha256 são obrigatórios".into()));
    }
    Ok(UpdateManifest {
        version: parsed.version.trim().trim_start_matches('v').to_string(),
        notes: parsed.notes,
        asset_name: parsed.setup.name,
        asset_url: parsed.setup.url,
        sha256: parsed.setup.sha256.trim().to_ascii_lowercase(),
        signature: parsed.setup.signature.filter(|s| !s.trim().is_empty()),
        checksum_url: None,
    })
}

pub fn parse_github_release(text: &str) -> Result<UpdateManifest, UpdateError> {
    let parsed: GithubRelease = serde_json::from_str(text).map_err(|e| UpdateError::Manifest(e.to_string()))?;
    let setup = parsed
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(SETUP_NAME))
        .ok_or_else(|| UpdateError::Manifest(format!("o release não tem {SETUP_NAME}")))?;
    let checksum = parsed.assets.iter().find(|a| a.name.eq_ignore_ascii_case("SHA256SUMS.txt"));
    Ok(UpdateManifest {
        version: parsed.tag_name.trim().trim_start_matches('v').to_string(),
        notes: parsed.body.unwrap_or_default(),
        asset_name: setup.name.clone(),
        asset_url: setup.browser_download_url.clone(),
        sha256: String::new(),
        signature: None,
        checksum_url: checksum.map(|a| a.browser_download_url.clone()),
    })
}

pub fn apply_checksum_file(manifest: &mut UpdateManifest, sums: &str) -> Result<(), UpdateError> {
    let hash = find_checksum(sums, &manifest.asset_name).ok_or_else(|| UpdateError::Manifest(format!("SHA256SUMS.txt não lista {}", manifest.asset_name)))?;
    manifest.sha256 = hash;
    if let Some(sig) = find_signature_line(sums, &manifest.asset_name) {
        manifest.signature = Some(sig);
    }
    Ok(())
}

pub fn find_checksum(sums: &str, filename: &str) -> Option<String> {
    for line in sums.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let hash = parts.next()?.to_ascii_lowercase();
        let name = parts.next()?.trim_start_matches('*');
        if name.eq_ignore_ascii_case(filename) && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(hash);
        }
    }
    None
}

fn find_signature_line(sums: &str, filename: &str) -> Option<String> {
    let marker = format!("# signature {filename} ");
    for line in sums.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&marker) {
            let sig = rest.trim();
            if !sig.is_empty() {
                return Some(sig.to_string());
            }
        }
    }
    None
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn verify_download(bytes: &[u8], expected_sha256: &str, signature_hex: Option<&str>, public_key_hex: Option<&str>) -> Result<(), UpdateError> {
    let expected = expected_sha256.trim().to_ascii_lowercase();
    if expected.is_empty() || sha256_hex(bytes) != expected {
        return Err(UpdateError::HashMismatch);
    }
    let Some(public_key_hex) = public_key_hex.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let Some(signature_hex) = signature_hex.map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(UpdateError::MissingSignature);
    };
    verify_signature(bytes, signature_hex, public_key_hex)
}

pub fn verify_signature(bytes: &[u8], signature_hex: &str, public_key_hex: &str) -> Result<(), UpdateError> {
    let key_bytes = hex::decode(public_key_hex.trim()).map_err(|_| UpdateError::BadSignature)?;
    let key_bytes: [u8; 32] = key_bytes.try_into().map_err(|_| UpdateError::BadSignature)?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|_| UpdateError::BadSignature)?;
    let sig_bytes = hex::decode(signature_hex.trim()).map_err(|_| UpdateError::BadSignature)?;
    let sig_bytes: [u8; 64] = sig_bytes.try_into().map_err(|_| UpdateError::BadSignature)?;
    let signature = Signature::from_bytes(&sig_bytes);
    key.verify(bytes, &signature).map_err(|_| UpdateError::BadSignature)
}

pub fn sign_bytes(bytes: &[u8], secret_hex: &str) -> Result<String, UpdateError> {
    use ed25519_dalek::Signer;
    use ed25519_dalek::SigningKey;
    let seed = hex::decode(secret_hex.trim()).map_err(|e| UpdateError::Message(e.to_string()))?;
    let seed: [u8; 32] = seed.try_into().map_err(|_| UpdateError::Message("a chave secreta tem que ter 32 bytes".into()))?;
    let key = SigningKey::from_bytes(&seed);
    Ok(hex::encode(key.sign(bytes).to_bytes()))
}

pub fn public_key_from_secret(secret_hex: &str) -> Result<String, UpdateError> {
    use ed25519_dalek::SigningKey;
    let seed = hex::decode(secret_hex.trim()).map_err(|e| UpdateError::Message(e.to_string()))?;
    let seed: [u8; 32] = seed.try_into().map_err(|_| UpdateError::Message("a chave secreta tem que ter 32 bytes".into()))?;
    let key = SigningKey::from_bytes(&seed);
    Ok(hex::encode(key.verifying_key().to_bytes()))
}

pub fn decide(current: &str, latest: &str) -> UpdateDecision {
    if version_is_newer(latest, current) {
        UpdateDecision::Available
    } else {
        UpdateDecision::UpToDate
    }
}

pub fn version_is_newer(latest: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.trim().trim_start_matches('v').split(['.', '-', '+']).filter_map(|p| p.parse::<u64>().ok()).collect()
    }
    let a = parts(latest);
    let b = parts(current);
    let n = a.len().max(b.len());
    for i in 0..n {
        let l = a.get(i).copied().unwrap_or(0);
        let c = b.get(i).copied().unwrap_or(0);
        if l != c {
            return l > c;
        }
    }
    false
}

pub fn current_version() -> &'static str {
    APP_VERSION
}

pub fn embedded_public_key() -> &'static str {
    EMBEDDED_PUBLIC_KEY_HEX.trim()
}

/// O backup e o log ficam na pasta de dados, fora da pasta `app` que o
/// instalador substitui.
pub fn persistent_paths(data_dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        data_dir.join("backup.json"),
        data_dir.join("log.jsonl"),
        data_dir.join("app").join("turbo-app.exe"),
    )
}

pub fn fetch_text(url: &str) -> Result<String, UpdateError> {
    let response = ureq::get(url)
        .timeout(Duration::from_secs(20))
        .set("User-Agent", &format!("TurboApp/{APP_VERSION}"))
        .set("Accept", "application/vnd.github+json, application/json;q=0.9, */*;q=0.8")
        .call()
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    response.into_string().map_err(|e| UpdateError::Network(e.to_string()))
}

pub fn fetch_bytes(url: &str) -> Result<Vec<u8>, UpdateError> {
    let response = ureq::get(url)
        .timeout(Duration::from_secs(120))
        .set("User-Agent", &format!("TurboApp/{APP_VERSION}"))
        .call()
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    let mut buf = Vec::new();
    response.into_reader().read_to_end(&mut buf).map_err(|e| UpdateError::Network(e.to_string()))?;
    Ok(buf)
}

pub fn load_manifest(url: &str) -> Result<UpdateManifest, UpdateError> {
    let text = fetch_text(url)?;
    if url.contains("api.github.com") && url.contains("/releases") {
        let mut manifest = parse_github_release(&text)?;
        if let Some(checksum_url) = manifest.checksum_url.clone() {
            let sums = fetch_text(&checksum_url)?;
            apply_checksum_file(&mut manifest, &sums)?;
        }
        if manifest.sha256.is_empty() {
            return Err(UpdateError::Manifest("o release não publicou SHA256SUMS.txt".into()));
        }
        return Ok(manifest);
    }
    parse_plain_manifest(&text)
}

/// Ambiente e arquivos locais substituem a URL embutida. Sem eles, vale o GitHub.
pub fn resolve_update_url(env_url: Option<&str>, file_urls: &[Option<String>]) -> String {
    if let Some(url) = env_url.map(str::trim).filter(|url| url.starts_with("http")) {
        return url.to_string();
    }
    for url in file_urls.iter().flatten() {
        let url = url.trim();
        if url.starts_with("http") {
            return url.to_string();
        }
    }
    DEFAULT_MANIFEST_URL.to_string()
}

pub fn read_update_source(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let url = text.lines().find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))?.trim().to_string();
    if url.starts_with("http://") || url.starts_with("https://") {
        Some(url)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn keypair() -> (String, String) {
        let secret = hex::encode([9u8; 32]);
        let public = public_key_from_secret(&secret).unwrap();
        (secret, public)
    }

    #[test]
    fn hash_and_signature_gate() {
        let (secret, public) = keypair();
        let bytes = b"instalador";
        let hash = sha256_hex(bytes);
        let sig = sign_bytes(bytes, &secret).unwrap();
        verify_download(bytes, &hash, Some(&sig), Some(&public)).unwrap();
        assert!(matches!(verify_download(b"outro", &hash, Some(&sig), Some(&public)), Err(UpdateError::HashMismatch)));
        assert!(matches!(verify_download(bytes, &hash, Some("aa"), Some(&public)), Err(UpdateError::BadSignature)));
        assert!(matches!(verify_download(bytes, &hash, None, Some(&public)), Err(UpdateError::MissingSignature)));
    }

    #[test]
    fn checksum_file_and_manifest_parse() {
        let hash = "ab".repeat(32);
        let sums = format!("{hash}  TurboApp-Setup.exe\n# signature TurboApp-Setup.exe cafebabe\n");
        assert_eq!(find_checksum(&sums, "TurboApp-Setup.exe").as_deref(), Some(hash.as_str()));
        let raw = r#"{
            "version": "1.2.0",
            "notes": "Corrige a leitura do Spooler.",
            "setup": {
                "name": "TurboApp-Setup.exe",
                "url": "https://example.com/TurboApp-Setup.exe",
                "sha256": "HASH",
                "signature": "sig"
            }
        }"#.replace("HASH", &hash);
        let manifest = parse_plain_manifest(&raw).unwrap();
        assert_eq!(manifest.version, "1.2.0");
        assert_eq!(manifest.signature.as_deref(), Some("sig"));
        assert_eq!(decide("1.0.0", &manifest.version), UpdateDecision::Available);
        assert_eq!(decide("1.2.0", &manifest.version), UpdateDecision::UpToDate);
        assert!(!version_is_newer("1.0.0", "1.0.1"));
    }

    #[test]
    fn github_release_parser_finds_the_setup_and_checksum() {
        let json = r#"{
            "tag_name": "v1.1.0",
            "body": "Notas do release",
            "assets": [
                {"name": "TurboApp-Setup.exe", "browser_download_url": "https://example.com/setup.exe"},
                {"name": "SHA256SUMS.txt", "browser_download_url": "https://example.com/SHA256SUMS.txt"}
            ]
        }"#;
        let mut manifest = parse_github_release(json).unwrap();
        assert_eq!(manifest.version, "1.1.0");
        assert_eq!(manifest.notes, "Notas do release");
        let hash = "cd".repeat(32);
        apply_checksum_file(&mut manifest, &format!("{hash} *TurboApp-Setup.exe\n")).unwrap();
        assert_eq!(manifest.sha256, hash);
    }

    #[test]
    fn default_manifest_is_the_public_github_release() {
        let url = resolve_update_url(None, &[None]);
        assert_eq!(url, DEFAULT_MANIFEST_URL);
        assert!(url.starts_with("https://github.com/NoShaar/turbo-app/"));
        assert!(url.ends_with("/latest.json"));
        let custom = resolve_update_url(Some("https://exemplo.test/latest.json"), &[None]);
        assert_eq!(custom, "https://exemplo.test/latest.json");
    }

    #[test]
    fn backup_lives_outside_the_replaced_app_folder() {
        let (backup, log, exe) = persistent_paths(Path::new(r"C:\Users\Carlos\AppData\Local\TurboApp"));
        assert!(backup.ends_with("backup.json"));
        assert!(log.ends_with("log.jsonl"));
        assert!(exe.ends_with("turbo-app.exe"));
        assert!(!backup.starts_with(exe.parent().unwrap()));
        assert_eq!(backup.parent(), log.parent());
        assert_ne!(backup.parent(), exe.parent());
    }

    #[test]
    fn signing_roundtrip_with_generated_key() {
        let secret = SigningKey::from_bytes(&[4u8; 32]);
        let public = hex::encode(secret.verifying_key().to_bytes());
        let data = b"turbo";
        let sig = sign_bytes(data, &hex::encode([4u8; 32])).unwrap();
        verify_signature(data, &sig, &public).unwrap();
    }
}
