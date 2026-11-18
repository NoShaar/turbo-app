//! Classificação portátil. Nada aqui depende de nome de adaptador, idioma do
//! Windows ou de um GUID que só existe num PC. Testes cobrem os cenários.

use crate::model::{
    normalize_guid, PowerSchemeInfo, PowerTier, BALANCED_GUID, HIGH_PERFORMANCE_GUID, POWER_SAVER_GUID, ULTIMATE_PERFORMANCE_GUID,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerClass {
    Ultimate,
    High,
    Balanced,
    Saver,
    Other,
}

impl PowerClass {
    pub fn is_good(self) -> bool {
        matches!(self, Self::Ultimate | Self::High)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ultimate => "Desempenho Máximo",
            Self::High => "Alto desempenho",
            Self::Balanced => "Equilibrado",
            Self::Saver => "Economia de energia",
            Self::Other => "plano não reconhecido",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PowerAction {
    AlreadyGood,
    Activate(String),
    Duplicate(String),
}

pub fn classify_power(scheme: &PowerSchemeInfo) -> PowerClass {
    let guid = normalize_guid(&scheme.guid);
    if guid == ULTIMATE_PERFORMANCE_GUID {
        return PowerClass::Ultimate;
    }
    if guid == HIGH_PERFORMANCE_GUID {
        return PowerClass::High;
    }
    if guid == BALANCED_GUID {
        return PowerClass::Balanced;
    }
    if guid == POWER_SAVER_GUID {
        return PowerClass::Saver;
    }

    let personality = scheme.personality;
    let cpu = scheme.min_processor_ac.unwrap_or(0);
    let disk_never = scheme.disk_timeout_ac == Some(0);
    let content_ultimate = disk_never && cpu >= 100 && !matches!(personality, Some(0));
    let content_high = cpu >= 100 && !matches!(personality, Some(0) | Some(2));
    let name = fold(&scheme.name);

    if content_ultimate {
        return PowerClass::Ultimate;
    }
    if name_is_ultimate(&name) && !matches!(personality, Some(0) | Some(2)) {
        return PowerClass::Ultimate;
    }
    if personality == Some(1) || content_high || (name_is_high(&name) && !matches!(personality, Some(0) | Some(2))) {
        return PowerClass::High;
    }
    if personality == Some(2) || name_is_balanced(&name) {
        return PowerClass::Balanced;
    }
    if personality == Some(0) || name_is_saver(&name) {
        return PowerClass::Saver;
    }
    PowerClass::Other
}

pub fn plan_power(active: &PowerSchemeInfo, schemes: &[PowerSchemeInfo], tier: PowerTier) -> PowerAction {
    if classify_power(active).is_good() {
        return PowerAction::AlreadyGood;
    }
    match tier {
        PowerTier::Ultimate => {
            if let Some(found) = schemes.iter().find(|s| classify_power(s) == PowerClass::Ultimate) {
                PowerAction::Activate(found.guid.clone())
            } else {
                PowerAction::Duplicate(ULTIMATE_PERFORMANCE_GUID.to_string())
            }
        }
        PowerTier::High => {
            if let Some(found) = schemes.iter().find(|s| normalize_guid(&s.guid) == HIGH_PERFORMANCE_GUID) {
                PowerAction::Activate(found.guid.clone())
            } else {
                PowerAction::Duplicate(HIGH_PERFORMANCE_GUID.to_string())
            }
        }
    }
}

pub fn guids_equal(left: &str, right: &str) -> bool {
    let a = normalize_guid(left);
    let b = normalize_guid(right);
    !a.is_empty() && a == b
}

pub fn is_builtin_scheme(guid: &str) -> bool {
    let guid = normalize_guid(guid);
    guid == ULTIMATE_PERFORMANCE_GUID || guid == HIGH_PERFORMANCE_GUID || guid == BALANCED_GUID || guid == POWER_SAVER_GUID
}

/// Keywords estáveis do driver. O texto "Activado" / "Enabled" / "Desactivado" não entra.
pub fn is_nic_power_keyword(keyword: &str) -> bool {
    let k = keyword.trim().trim_start_matches('*').trim().to_ascii_lowercase();
    matches!(k.as_str(), "eee" | "powersavingmode" | "enablegreenethernet" | "greenethernet" | "advancedeee" | "eeelinkadvertisement")
}

/// Ligado quando o valor numérico não é 0. Display localizado não conta.
pub fn nic_power_enabled(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() {
        return false;
    }
    let digits = v.trim_start_matches("0x").trim_start_matches("0X");
    match u32::from_str_radix(digits, if v.to_ascii_lowercase().starts_with("0x") { 16 } else { 10 }) {
        Ok(n) => n != 0,
        Err(_) => false,
    }
}

/// Teto só quando a descrição do driver traz um token de produto (GbE, I225…),
/// não quando o nome da conexão é "Ethernet".
pub fn inferred_link_cap_bps(description: &str) -> Option<u64> {
    let d = description.to_ascii_lowercase();
    if d.contains("10gbe") || d.contains("10g base") || d.contains("10 gig") {
        Some(10_000_000_000)
    } else if d.contains("5gbe") || d.contains("5 gig") {
        Some(5_000_000_000)
    } else if d.contains("2.5g") || d.contains("i225") || d.contains("i226") {
        Some(2_500_000_000)
    } else if d.contains("gbe") || d.contains("gigabit") {
        Some(1_000_000_000)
    } else {
        None
    }
}

pub fn health_is_bad(health: &str) -> bool {
    let h = fold(health);
    if h.is_empty() {
        return false;
    }
    h.contains("unhealthy") || h.contains("fail") || h.contains("predict") || h.contains("warning") || h == "bad"
}

fn name_is_ultimate(name: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "ultimate performance",
        "desempenho maximo",
        "rendimiento maximo",
        "maximo rendimiento",
        "ultimative leistung",
        "performances ultimes",
        "prestazioni eccellenti",
    ];
    NEEDLES.iter().any(|n| name.contains(n))
}

fn name_is_high(name: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "high performance",
        "alto desempenho",
        "alto rendimiento",
        "hohe leistung",
        "performances elevees",
        "prestazioni elevate",
    ];
    NEEDLES.iter().any(|n| name.contains(n))
}

fn name_is_balanced(name: &str) -> bool {
    const NEEDLES: &[&str] = &["balanced", "equilibrado", "equilibre", "ausgewogen", "equilibrata"];
    NEEDLES.iter().any(|n| name.contains(n))
}

fn name_is_saver(name: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "power saver",
        "economia de energia",
        "economizador",
        "ahorrador",
        "energiespar",
        "economie d'energie",
        "risparmio energia",
    ];
    NEEDLES.iter().any(|n| name.contains(n))
}

pub fn fold(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'Á' | 'À' | 'Â' | 'Ã' | 'á' | 'à' | 'â' | 'ã' => 'a',
            'É' | 'Ê' | 'é' | 'ê' => 'e',
            'Í' | 'í' => 'i',
            'Ó' | 'Ô' | 'Õ' | 'ó' | 'ô' | 'õ' => 'o',
            'Ú' | 'ú' => 'u',
            'Ç' | 'ç' => 'c',
            other => other.to_ascii_lowercase(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheme(guid: &str, name: &str, personality: Option<u32>, disk: Option<u32>, cpu: Option<u32>) -> PowerSchemeInfo {
        PowerSchemeInfo { guid: guid.into(), name: name.into(), personality, disk_timeout_ac: disk, min_processor_ac: cpu }
    }

    #[test]
    fn duplicated_ultimate_is_not_a_custom_downgrade() {
        let owner = scheme("4698c0e9-058a-42c5-b251-951cc5e9db6a", "Desempenho Máximo", Some(1), Some(0), Some(100));
        assert_eq!(classify_power(&owner), PowerClass::Ultimate);
        let renamed = scheme("{4698C0E9-058A-42C5-B251-951CC5E9DB6A}", "", Some(1), Some(0), Some(100));
        assert_eq!(classify_power(&renamed), PowerClass::Ultimate);
        let name_only = scheme("4698c0e9-058a-42c5-b251-951cc5e9db6a", "Desempenho Máximo", None, None, None);
        assert_eq!(classify_power(&name_only), PowerClass::Ultimate);
    }

    #[test]
    fn high_duplicate_and_other_languages_stay_ok() {
        let high_copy = scheme("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "Alto desempenho", Some(1), Some(1200), Some(100));
        assert_eq!(classify_power(&high_copy), PowerClass::High);
        let spanish = scheme("bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee", "Máximo rendimiento", Some(1), None, None);
        assert_eq!(classify_power(&spanish), PowerClass::Ultimate);
        let oem = scheme("cccccccc-bbbb-cccc-dddd-eeeeeeeeeeee", "OEM", Some(1), Some(1200), Some(100));
        assert_eq!(classify_power(&oem), PowerClass::High);
        assert!(classify_power(&scheme(HIGH_PERFORMANCE_GUID, "", None, Some(0), Some(100))).is_good());
    }

    #[test]
    fn contradictory_name_loses_to_personality() {
        let liar = scheme("dddddddd-bbbb-cccc-dddd-eeeeeeeeeeee", "Alto desempenho", Some(2), Some(1200), Some(5));
        assert_eq!(classify_power(&liar), PowerClass::Balanced);
    }

    #[test]
    fn balanced_plans_ultimate_first_and_never_replaces_a_better_plan() {
        let balanced = scheme(BALANCED_GUID, "Equilibrado", Some(2), Some(1200), Some(5));
        let ultimate = scheme("4698c0e9-058a-42c5-b251-951cc5e9db6a", "Desempenho Máximo", Some(1), Some(0), Some(100));
        assert_eq!(plan_power(&balanced, &[balanced.clone(), ultimate.clone()], PowerTier::Ultimate), PowerAction::Activate(ultimate.guid.clone()));
        assert_eq!(
            plan_power(&balanced, &[balanced.clone()], PowerTier::Ultimate),
            PowerAction::Duplicate(ULTIMATE_PERFORMANCE_GUID.into())
        );
        assert_eq!(plan_power(&ultimate, &[ultimate.clone()], PowerTier::High), PowerAction::AlreadyGood);
        assert_eq!(plan_power(&balanced, &[balanced.clone()], PowerTier::High), PowerAction::Duplicate(HIGH_PERFORMANCE_GUID.into()));
    }

    #[test]
    fn realtek_guid_match_ignores_case_and_braces() {
        assert!(guids_equal("{C8E0BA2D-28FA-4DB2-91F4-68CC507F35BF}", "c8e0ba2d-28fa-4db2-91f4-68cc507f35bf"));
        assert!(!guids_equal("Ethernet", "{C8E0BA2D-28FA-4DB2-91F4-68CC507F35BF}"));
    }

    #[test]
    fn nic_keywords_ignore_localized_display() {
        assert!(is_nic_power_keyword("*EEE"));
        assert!(is_nic_power_keyword("EnableGreenEthernet"));
        assert!(is_nic_power_keyword("PowerSavingMode"));
        assert!(!is_nic_power_keyword("DriverDesc"));
        assert!(nic_power_enabled("1"));
        assert!(!nic_power_enabled("0"));
        assert!(!nic_power_enabled("Activado"));
        assert!(!nic_power_enabled("Desactivado"));
    }

    #[test]
    fn link_cap_uses_product_tokens_not_the_connection_name() {
        assert_eq!(inferred_link_cap_bps("Realtek PCIe GbE Family Controller"), Some(1_000_000_000));
        assert_eq!(inferred_link_cap_bps("Intel(R) Ethernet Controller I226-V"), Some(2_500_000_000));
        assert_eq!(inferred_link_cap_bps("Ethernet"), None);
        assert_eq!(inferred_link_cap_bps("Wi-Fi"), None);
    }
}
