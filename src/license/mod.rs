//! License Gate POLER Engine (v0.18.0).
//!
//! Философия гейта — «вдумчиво» означает три принципа:
//!
//! 1. ЛОКАЛЬНОЕ — СВЯТО. Поиск, резонанс, AIDDE, граф, TUI/shell работают
//!    ВСЕГДА и БЕЗ лицензии. Гейтится только доступ к платным ИНТЕГРАЦИЯМ
//!    (Gmail / Drive / NotebookLM через OAuth и профиль браузера).
//!    Пользователь никогда не теряет доступ к своим данным и локальному
//!    движку — «кирпич» невозможен по построению.
//!
//! 2. ОФЛАЙН-ЧЕСТНОСТЬ. Лицензия = JSON {product, name, email, tier,
//!    issued, expires, features} + подпись ed25519 мастер-ключом POLER.
//!    Проверка подписи — локально, за микросекунды, без сервера, без
//!    телеметрии, без «звонков домой». Мы доверяем математике, а не сети.
//!
//! 3. МЯГКИЕ ПРЕДЕЛЫ. Community (без ключа): локальное — без лимитов,
//!    интеграции — 50 операций за скользящие 24 ч (щедрый осмотр).
//!    Trial: первые 14 дней после первого запуска — все функции.
//!    Истёкшая лицензия: 7 дней grace-периода с предупреждением, затем
//!    тихий откат на Community — никогда не блокируем и не удаляем ничего.
//!
//! Формат ключа активации (одна строка, почта-friendly):
//!   PO1.<base64url(payload JSON)>.<base64url(подпись 64 байта)>
//!
//! Приватный мастер-ключ живёт офлайн у владельца (license-tool,
//! отдельный крейт, в поставку не входит). Здесь — только ПУБЛИЧНЫЙ ключ:
//! подделать лицензию без приватного ключа — это задача дискретного
//! логарифма на ed25519, а не «поменять байтик в файле».

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Публичный мастер-ключ POLER (ed25519, 32 байта). Приватный — офлайн
/// у владельца. Сгенерирован poler-license-tool keygen 2026-08-29.
pub const POLER_LICENSE_PUBLIC_KEY_HEX: &str =
    "f44efe7f62b2c436d66a6fd3daff500e409820819e03a31f6b2066860907d5f6";

/// Community: операций интеграций за скользящие 24 часа.
pub const COMMUNITY_DAILY_OPS: u32 = 50;
/// Trial: дней полного доступа с первого запуска движка.
pub const TRIAL_DAYS: u64 = 14;
/// Grace-период после окончания лицензии (дней), затем откат на Community.
pub const EXPIRY_GRACE_DAYS: u64 = 7;

/// Имена гейтируемых функций (передаются в `gate`).
pub const FEATURE_GMAIL: &str = "gmail";
pub const FEATURE_DRIVE: &str = "drive";
pub const FEATURE_NLM: &str = "nlm";

// ---------- v0.22.0: Source-Available EULA (Unreal Engine модель) ----------

/// Название лицензионной модели (LICENSE.md — юридический инструмент).
pub const EULA_NAME: &str =
    "POLER Custom Source-Available & Modification Disclosure License v1.0";
/// Обязательный адрес раскрытия модификаций (Notification Clause, §4).
pub const MODIFICATION_NOTICE_EMAIL: &str = "dev@poler-engine.org";
/// Официальный репозиторий (для ссылок в баннере/CLI).
pub const EULA_REPO_URL: &str = "https://github.com/poler-engine-org/poler-engine";

/// ЕДИНЫЙ блок EULA-статуса: печатается в `--license`, в gateway-баннере
/// и в команде `license` REPL (один источник правды — no drift).
///
/// Модель: Source-Available (Unreal Engine EULA precedent) — исходники
/// открыты для изучения/сборки/модификации, но модификации при
/// дистрибуции/деплое продукта подлежат обязательному уведомлению
/// авторов (14 дней), редистрибуция ядра и обход Ed25519-гейта запрещены.
pub fn eula_notice() -> String {
    let mut s = String::new();
    s.push_str(&format!("  Модель:       {EULA_NAME}\n"));
    s.push_str(&format!(
        "  Раскрытие:    модификации при дистрибуции/деплое — уведомить\n                {} в течение 14 дней (Notification Clause)\n",
        MODIFICATION_NOTICE_EMAIL
    ));
    s.push_str("  Условия:      LICENSE.md · TERMS.md (в корне репозитория)\n");
    s.push_str(&format!("  Репозиторий:  {EULA_REPO_URL}\n"));
    s.push_str("  Запрещено:    редистрибуция ядра, обход Ed25519 License Gate\n");
    s
}

/// Короткая строка для баннера запуска (одна строка, без блоков).
pub fn eula_banner_line() -> String {
    format!(
        "Лицензия: {EULA_NAME} — модификации подлежат раскрытию → {MODIFICATION_NOTICE_EMAIL} (TERMS.md)"
    )
}

/// Полный текст статуса лицензии как String (v0.22.0): один источник
/// для CLI `--license` и команды `license` в Terminal Gateway.
pub fn status_text() -> String {
    let st = status();
    let mut s = String::from("POLER Engine — лицензия\n\n");
    match st.tier {
        Tier::Trial => s.push_str(&format!(
            "  Тир:         Trial — все функции, {} дн. осталось\n",
            st.trial_days_left
        )),
        Tier::Community => s.push_str("  Тир:         Community (без лицензии)\n"),
        Tier::Pro => s.push_str("  Тир:         Pro\n"),
        Tier::Enterprise => s.push_str("  Тир:         Enterprise\n"),
    }
    if let Some(lic) = &st.license {
        s.push_str(&format!(
            "  Владелец:    {} <{}>\n",
            lic.name, lic.email
        ));
        s.push_str(&format!("  Выдана:      {}\n", civil_date(lic.issued)));
        if lic.expires == 0 {
            s.push_str("  Срок:        бессрочно\n");
        } else {
            s.push_str(&format!(
                "  Действует до: {}\n",
                civil_date(lic.expires)
            ));
        }
        if let Some(g) = st.grace_days_left {
            if g > 0 {
                s.push_str(&format!(
                    "  ⚠ Истекла — grace-период: {g} дн., затем Community-лимиты\n"
                ));
            } else if st.tier == Tier::Community {
                s.push_str("  ⚠ Истекла сверх grace — работаем на Community-лимитах\n");
            }
        }
        if !lic.features.is_empty() {
            s.push_str(&format!("  Функции:     {}\n", lic.features.join(", ")));
        }
    } else {
        s.push_str("  Локальный поиск, резонанс, AIDDE, граф, TUI: БЕЗ лимитов\n");
        for (feat, used) in &st.quota_used {
            s.push_str(&format!(
                "  Интеграция {feat}: {}/{} операций за 24 ч\n",
                used, COMMUNITY_DAILY_OPS
            ));
        }
        if st.trial_days_left > 0 {
            s.push_str(&format!(
                "  Trial полных функций: {} дн. осталось\n",
                st.trial_days_left
            ));
        }
    }
    match &st.source {
        Some(src) => s.push_str(&format!("  Источник:    {src}\n")),
        None => s.push_str("  Источник:    лицензии нет\n"),
    }
    s.push_str("\n  Активация: poler-engine --license-import PO1.….….  (ключ одной строкой)\n");
    s.push_str("\nLicensing (v0.22.0 — Source-Available, Unreal Engine EULA модель):\n");
    s.push_str(&eula_notice());
    s
}

// ---------- ошибки ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LicenseError {
    BadFormat(&'static str),
    BadBase64(String),
    BadSignature,
    BadPayload(String),
}

impl std::fmt::Display for LicenseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LicenseError::BadFormat(m) => write!(f, "неверный формат ключа: {m}"),
            LicenseError::BadBase64(m) => write!(f, "ключ повреждён (base64url): {m}"),
            LicenseError::BadSignature => write!(
                f,
                "ПОДПИСЬ НЕ ПРОШЛА: ключ не выпускался POLER или повреждён"
            ),
            LicenseError::BadPayload(m) => write!(f, "ключ подписан, но некорректен: {m}"),
        }
    }
}

impl std::error::Error for LicenseError {}

// ---------- лицензия ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Без лицензии: локальное — всё, интеграции — дневная квота.
    Community,
    /// Первые TRIAL_DAYS дней после первого запуска: всё открыто.
    Trial,
    /// Pro-лицензия.
    Pro,
    /// Enterprise (бессрочная разрешена).
    Enterprise,
}

impl Tier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Community => "community",
            Tier::Trial => "trial",
            Tier::Pro => "pro",
            Tier::Enterprise => "enterprise",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct License {
    pub product: String,
    pub name: String,
    pub email: String,
    pub tier: String,
    pub issued: u64,
    /// 0 = бессрочно (разрешено только enterprise).
    pub expires: u64,
    #[serde(default)]
    pub features: Vec<String>,
}

// ---------- base64url (без паддинга, RFC 4648 §5) ----------

const B64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn b64url_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL_ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(B64URL_ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(B64URL_ALPHABET[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL_ALPHABET[n as usize & 63] as char);
        }
    }
    out
}

pub fn b64url_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4 + 3);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for (i, ch) in s.chars().enumerate() {
        let v = B64URL_ALPHABET
            .iter()
            .position(|&c| c as char == ch)
            .ok_or_else(|| format!("недопустимый символ на позиции {i}: {ch:?}"))? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}

// ---------- парсинг и проверка ключа ----------

fn master_public_key() -> Result<VerifyingKey, LicenseError> {
    let mut pk = [0u8; 32];
    let hex = POLER_LICENSE_PUBLIC_KEY_HEX;
    for i in 0..32 {
        pk[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| LicenseError::BadFormat("встроенный мастер-ключ повреждён"))?;
    }
    VerifyingKey::from_bytes(&pk).map_err(|_| LicenseError::BadFormat("встроенный мастер-ключ повреждён"))
}

/// Полный разбор PO1-ключа: формат → base64 → подпись → семантика.
/// Подпись проверяется по СЫРЫМ байтам payload (порядок полей не важен).
pub fn parse_key(key: &str) -> Result<License, LicenseError> {
    let key = key.trim();
    let parts: Vec<&str> = key.split('.').collect();
    if parts.len() != 3 || parts[0] != "PO1" {
        return Err(LicenseError::BadFormat(
            "ожидается PO1.<payload>.<подпись> (одна строка)",
        ));
    }
    let payload_b = b64url_decode(parts[1]).map_err(LicenseError::BadBase64)?;
    let sig_b = b64url_decode(parts[2]).map_err(LicenseError::BadBase64)?;
    if sig_b.len() != 64 {
        return Err(LicenseError::BadFormat("подпись должна быть 64 байта"));
    }
    let mut sig_arr = [0u8; 64];
    sig_arr.copy_from_slice(&sig_b);
    let sig = Signature::from_bytes(&sig_arr);

    let vk = master_public_key()?;
    // Аудит-фикс №D1: verify_strict вместо verify — отклоняет
    // неканоничные (malleable) подписи: S >= l, small-order R/A.
    // Для целостности лицензии достаточно и verify, но каноничность
    // подписи — бесплатно закрывает целый класс атак на подписи.
    vk.verify_strict(&payload_b, &sig)
        .map_err(|_| LicenseError::BadSignature)?;

    let lic: License = serde_json::from_slice(&payload_b)
        .map_err(|e| LicenseError::BadPayload(e.to_string()))?;
    validate_license(&lic, now_unix())?;
    Ok(lic)
}

fn validate_license(lic: &License, now: u64) -> Result<(), LicenseError> {
    if lic.product != "poler-engine" {
        return Err(LicenseError::BadPayload("ключ не для poler-engine".to_string()));
    }
    if !matches!(lic.tier.as_str(), "community" | "pro" | "enterprise") {
        return Err(LicenseError::BadPayload(format!("неизвестный тир: {}", lic.tier)));
    }
    if lic.name.trim().is_empty() || !lic.email.contains('@') {
        return Err(LicenseError::BadPayload("пустое имя или email".to_string()));
    }
    // 10 минут допуска на рассинхрон часов.
    if lic.issued > now + 600 {
        return Err(LicenseError::BadPayload("дата выдачи в будущем (часы?)".to_string()));
    }
    if lic.expires == 0 {
        if lic.tier != "enterprise" {
            return Err(LicenseError::BadPayload(
                "бессрочная лицензия разрешена только enterprise".to_string(),
            ));
        }
    } else if lic.expires <= lic.issued {
        return Err(LicenseError::BadPayload("срок действия раньше выдачи".to_string()));
    }
    Ok(())
}

// ---------- хранение ----------

/// Конфиг-директория лицензий = конфиг движка (`$POLER_CONFIG_DIR` →
/// `~/.config/poler-engine`) — единая ручка для тестов и песочниц.
fn license_file() -> PathBuf {
    crate::google::config_dir().join("license.key")
}

fn state_file() -> PathBuf {
    crate::google::config_dir().join("license-state.json")
}

/// Откуда движок берёт ключ (приоритет): env POLER_LICENSE_KEY →
/// env POLER_LICENSE_FILE → ~/.config/poler-engine/license.key.
fn read_stored_raw() -> Option<String> {
    if let Ok(k) = std::env::var("POLER_LICENSE_KEY") {
        if !k.trim().is_empty() {
            return Some(k);
        }
    }
    if let Ok(p) = std::env::var("POLER_LICENSE_FILE") {
        if let Ok(s) = fs::read_to_string(p.trim()) {
            let s = s.trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    fs::read_to_string(license_file())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ---------- состояние (trial + квоты) ----------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct QuotaEntry {
    /// Начало скользящего окна 24 ч (unix sec).
    w: u64,
    /// Счётчик операций в окне.
    n: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LicenseState {
    /// Первый запуск движка (для trial).
    #[serde(default)]
    first_run: u64,
    /// Счётчики по фичам: {"gmail": {"w":..,"n":..}, ...}
    #[serde(default)]
    quota: std::collections::BTreeMap<String, QuotaEntry>,
}

impl LicenseState {
    fn load() -> LicenseState {
        fs::read_to_string(state_file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let dir = crate::google::config_dir();
        let _ = fs::create_dir_all(&dir);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(state_file(), json);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(state_file(), fs::Permissions::from_mode(0o600));
            }
        }
    }

    /// Гарантирует, что first_run зафиксирован (создаёт при первом обращении).
    fn ensure_first_run(&mut self) {
        if self.first_run == 0 {
            self.first_run = now_unix();
            self.save();
        }
    }

    fn trial_days_left(&self, now: u64) -> u64 {
        if self.first_run == 0 {
            return TRIAL_DAYS;
        }
        let elapsed = now.saturating_sub(self.first_run) / 86400;
        TRIAL_DAYS.saturating_sub(elapsed)
    }

    fn used_and_bump(&mut self, feature: &str, now: u64) -> u32 {
        let e = self.quota.entry(feature.to_string()).or_default();
        if now.saturating_sub(e.w) >= 86400 {
            e.w = now;
            e.n = 0;
        }
        e.n = e.n.saturating_add(1);
        let n = e.n;
        self.save();
        n
    }

    fn used_current(&self, feature: &str, now: u64) -> u32 {
        match self.quota.get(feature) {
            Some(e) if now.saturating_sub(e.w) < 86400 => e.n,
            _ => 0,
        }
    }
}

// ---------- публичный API ----------

#[derive(Debug, Clone)]
pub struct Status {
    pub tier: Tier,
    pub license: Option<License>,
    /// дней осталось у лицензии (0 = бессрочно/нет).
    pub license_days_left: Option<u64>,
    /// дней grace после истечения (если применимо).
    pub grace_days_left: Option<u64>,
    pub trial_days_left: u64,
    /// (использовано, лимит) за текущее окно 24 ч по фичам.
    pub quota_used: std::collections::BTreeMap<String, u32>,
    pub source: Option<String>,
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Полная сводка для `--license`.
pub fn status() -> Status {
    let now = now_unix();
    let mut st = LicenseState::load();
    st.ensure_first_run();
    let raw = read_stored_raw();
    let parsed = raw.as_deref().map(parse_key).and_then(|r| r.ok());

    let mut quota_used = std::collections::BTreeMap::new();
    for f in [FEATURE_GMAIL, FEATURE_DRIVE, FEATURE_NLM] {
        quota_used.insert(f.to_string(), st.used_current(f, now));
    }

    let source = if std::env::var("POLER_LICENSE_KEY").map(|v| !v.trim().is_empty()).unwrap_or(false) {
        Some("env POLER_LICENSE_KEY".to_string())
    } else if let Ok(p) = std::env::var("POLER_LICENSE_FILE") {
        Some(format!("env POLER_LICENSE_FILE={}", p.trim()))
    } else if raw.is_some() {
        Some(license_file().display().to_string())
    } else {
        None
    };

    match parsed {
        Some(lic) => {
            let (tier, days_left, grace) = effective_tier(&lic, now);
            Status {
                tier,
                license: Some(lic),
                license_days_left: days_left,
                grace_days_left: grace,
                trial_days_left: st.trial_days_left(now),
                quota_used,
                source,
            }
        }
        None => {
            let tier = if st.trial_days_left(now) > 0 {
                Tier::Trial
            } else {
                Tier::Community
            };
            Status {
                tier,
                license: None,
                license_days_left: None,
                grace_days_left: None,
                trial_days_left: st.trial_days_left(now),
                quota_used,
                source,
            }
        }
    }
}

/// Эффективный тир лицензии с учётом истечения и grace-периода.
fn effective_tier(lic: &License, now: u64) -> (Tier, Option<u64>, Option<u64>) {
    let base = match lic.tier.as_str() {
        "enterprise" => Tier::Enterprise,
        _ => Tier::Pro,
    };
    if lic.expires == 0 {
        return (base, Some(0), None); // бессрочно
    }
    if now < lic.expires {
        (base, Some((lic.expires - now) / 86400 + 1), None)
    } else {
        let over = (now - lic.expires) / 86400;
        if over < EXPIRY_GRACE_DAYS {
            (base, Some(0), Some(EXPIRY_GRACE_DAYS - over))
        } else {
            (Tier::Community, Some(0), Some(0)) // откат, но статус покажет детали
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateResult {
    /// Разрешено (+тир). Предупреждение (если есть) уже напечатано в stderr.
    Allowed(Tier),
    /// Community-квота исчерпана: использовано/лимит.
    QuotaExhausted { used: u32, max: u32 },
}

/// Главный гейт. Вызывать ПЕРЕД платной операцией интеграции.
/// Пропускает Pro/Enterprise/Trial; Community — пока квота не исчерпана.
/// Печатает в stderr дружелюбное пояснение при истечении/лимите
/// (одна строка на вызов CLI — не спамит).
pub fn gate(feature: &str) -> GateResult {
    let now = now_unix();
    let mut st = LicenseState::load();
    st.ensure_first_run();
    let raw = read_stored_raw();
    let parsed = raw.as_deref().map(parse_key).and_then(|r| r.ok());

    if let Some(lic) = parsed {
        let (tier, _, grace) = effective_tier(&lic, now);
        match tier {
            Tier::Pro | Tier::Enterprise => {
                if let Some(g) = grace.filter(|g| *g > 0) {
                    eprintln!(
                        "poler-engine: лицензия истекла, grace-период — ещё {g} дн. (потом Community-лимиты)"
                    );
                }
                return GateResult::Allowed(tier);
            }
            _ => {} // истекла сверх grace → падаем вниз на Community-квоту
        }
    }

    // Trial: полные функции без квот.
    if st.trial_days_left(now) > 0 {
        return GateResult::Allowed(Tier::Trial);
    }

    // Community: скользящая квота на интеграции.
    let n = st.used_and_bump(feature, now);
    if n > COMMUNITY_DAILY_OPS {
        eprintln!(
            "poler-engine: Community-лимит Google/NotebookLM исчерпан ({}/{} за 24 ч).",
            n - 1, COMMUNITY_DAILY_OPS
        );
        eprintln!(
            "Локальный поиск работает без ограничений. Статус: poler-engine --license"
        );
        eprintln!("Лицензия Pro: https://github.com/poler-engine-org");
        return GateResult::QuotaExhausted {
            used: n - 1,
            max: COMMUNITY_DAILY_OPS,
        };
    }
    GateResult::Allowed(Tier::Community)
}

/// Удобная обёртка для CLI/MCP: true = операция разрешена.
pub fn gate_or_print(feature: &str) -> bool {
    matches!(gate(feature), GateResult::Allowed(_))
}

/// Активация: принимает ГОТОВЫЙ PO1-ключ или ПУТЬ к файлу с ключом.
/// Проверяет подпись ДО сохранения; пишет 0600 в конфиг-директорию.
pub fn import(key_or_path: &str) -> Result<License, LicenseError> {
    let s = key_or_path.trim();
    let key = if s.starts_with("PO1.") {
        s.to_string()
    } else {
        fs::read_to_string(s)
            .map_err(|_| {
                LicenseError::BadFormat(
                    "аргумент — не PO1-ключ и не читаемый файл с ключом",
                )
            })?
            .trim()
            .to_string()
    };
    let lic = parse_key(&key)?; // подпись + семантика ДО записи на диск
    let dir = crate::google::config_dir();
    fs::create_dir_all(&dir).map_err(|e| {
        LicenseError::BadPayload(format!("не создать {}: {e}", dir.display()))
    })?;
    fs::write(license_file(), &key).map_err(|e| {
        LicenseError::BadPayload(format!("не записать {}: {e}", license_file().display()))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(license_file(), fs::Permissions::from_mode(0o600));
    }
    Ok(lic)
}

// ---------- даты (для человекочитаемого вывода) ----------

/// unix-секунды → YYYY-MM-DD (UTC), алгоритм Говарда Хиннанта.
pub fn civil_date(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// ---------- тесты ----------

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    /// Детерминированный тестовый ключ (не мастер!): seed = 7.
    fn test_signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn make_key(sk: &SigningKey, payload: &str) -> String {
        let sig = sk.sign(payload.as_bytes());
        format!(
            "PO1.{}.{}",
            b64url_encode(payload.as_bytes()),
            b64url_encode(&sig.to_bytes())
        )
    }

    fn valid_payload(now: u64) -> String {
        format!(
            "{{\"product\":\"poler-engine\",\"name\":\"Тест Юзер\",\"email\":\"a@b.co\",\
             \"tier\":\"pro\",\"issued\":{},\"expires\":{},\"features\":[\"gmail\",\"drive\",\"nlm\"]}}",
            now - 100,
            now + 86_400
        )
    }

    // base64url

    #[test]
    fn b64url_roundtrip_all_alphabet() {
        // Все 256 байт + все выравнивания длины.
        let data: Vec<u8> = (0..=255u8).collect();
        let enc = b64url_encode(&data);
        assert!(enc.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_eq!(b64url_decode(&enc).unwrap(), data);
    }

    #[test]
    fn b64url_lengths_0_1_2_3() {
        for len in 0..=3 {
            let data = vec![0xAB; len];
            let enc = b64url_encode(&data);
            assert_eq!(b64url_decode(&enc).unwrap(), data);
        }
    }

    #[test]
    fn b64url_known_vectors() {
        assert_eq!(b64url_encode(b""), "");
        assert_eq!(b64url_encode(b"f"), "Zg");
        assert_eq!(b64url_encode(b"fo"), "Zm8");
        assert_eq!(b64url_encode(b"foo"), "Zm9v");
        assert_eq!(b64url_encode(b"foob"), "Zm9vYg");
        assert_eq!(b64url_encode(b"fooba"), "Zm9vYmE");
        assert_eq!(b64url_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn b64url_rejects_standard_alphabet_and_padding() {
        assert!(b64url_decode("Zm9v+").is_err()); // '+' из RFC 4648 §4
        assert!(b64url_decode("Zm9v/").is_err()); // '/'
        assert!(b64url_decode("Zg==").is_err()); // паддинг
        assert!(b64url_decode("Zg=").is_err());
    }

    // parse_key — доверие только подписи мастера

    #[test]
    fn parse_rejects_wrong_format() {
        assert!(matches!(parse_key("hello"), Err(LicenseError::BadFormat(_))));
        assert!(matches!(parse_key("PO1.only"), Err(LicenseError::BadFormat(_))));
        assert!(matches!(parse_key("XX1.a.b"), Err(LicenseError::BadFormat(_))));
    }

    #[test]
    fn parse_rejects_foreign_signature() {
        // Подписано ТЕСТОВЫМ ключом, а не мастер-ключом → отказ.
        let now = now_unix();
        let key = make_key(&test_signing_key(), &valid_payload(now));
        assert_eq!(parse_key(&key), Err(LicenseError::BadSignature));
    }

    #[test]
    fn parse_rejects_tampered_payload() {
        // Подпись от одного payload, байты подменены.
        let sk = test_signing_key();
        let now = now_unix();
        let sig = sk.sign(valid_payload(now).as_bytes());
        let evil = valid_payload(now).replace("\"pro\"", "\"enterprise\"");
        let key = format!(
            "PO1.{}.{}",
            b64url_encode(evil.as_bytes()),
            b64url_encode(&sig.to_bytes())
        );
        assert_eq!(parse_key(&key), Err(LicenseError::BadSignature));
    }

    #[test]
    fn validate_rejects_wrong_product_and_tier() {
        let now = now_unix();
        let mut lic = parse_json(&valid_payload(now));
        lic.product = "other-engine".into();
        assert!(validate_license(&lic, now).is_err());
        lic.tier = "ultimate".into();
        assert!(validate_license(&lic, now).is_err());
    }

    #[test]
    fn validate_rejects_perpetual_pro() {
        let now = now_unix();
        let mut lic = parse_json(&valid_payload(now));
        lic.expires = 0; // бессрочная — только enterprise
        assert!(validate_license(&lic, now).is_err());
        lic.tier = "enterprise".into();
        assert!(validate_license(&lic, now).is_ok());
    }

    #[test]
    fn validate_rejects_future_issued_and_bad_dates() {
        let now = now_unix();
        let mut lic = parse_json(&valid_payload(now));
        lic.issued = now + 100_000; // выдана «в будущем» за пределами допуска
        assert!(validate_license(&lic, now).is_err());
        lic.issued = now - 100;
        lic.expires = lic.issued - 1; // кончилась раньше, чем выдана
        assert!(validate_license(&lic, now).is_err());
    }

    #[test]
    fn validate_allows_small_clock_skew() {
        let now = now_unix();
        let mut lic = parse_json(&valid_payload(now));
        lic.issued = now + 300; // 5 минут вперёд — в пределах допуска 10 мин
        assert!(validate_license(&lic, now).is_ok());
    }

    // effective_tier / grace

    #[test]
    fn effective_tier_active_pro() {
        let now = now_unix();
        let lic = parse_json(&valid_payload(now));
        let (t, days, g) = effective_tier(&lic, now);
        assert_eq!(t, Tier::Pro);
        assert_eq!(days, Some(2)); // ~1.99 дня → +1
        assert_eq!(g, None);
    }

    #[test]
    fn effective_tier_grace_then_community() {
        let now = now_unix();
        let mut lic = parse_json(&valid_payload(now));
        lic.expires = now - 3 * 86400; // истекла 3 дня назад
        let (t, _, g) = effective_tier(&lic, now);
        assert_eq!(t, Tier::Pro); // ещё в grace
        assert_eq!(g, Some(EXPIRY_GRACE_DAYS - 3));
        lic.expires = now - 30 * 86400; // истекла месяц назад
        let (t, _, _) = effective_tier(&lic, now);
        assert_eq!(t, Tier::Community);
    }

    #[test]
    fn effective_tier_perpetual_enterprise() {
        let now = now_unix();
        let mut lic = parse_json(&valid_payload(now));
        lic.tier = "enterprise".into();
        lic.expires = 0;
        let (t, days, _) = effective_tier(&lic, now);
        assert_eq!(t, Tier::Enterprise);
        assert_eq!(days, Some(0)); // 0 = бессрочно
    }

    // LicenseState: trial и квоты

    #[test]
    fn trial_counts_down_from_first_run() {
        let mut st = LicenseState::default();
        st.first_run = now_unix();
        assert_eq!(st.trial_days_left(now_unix()), TRIAL_DAYS);
        st.first_run = now_unix() - 5 * 86400;
        assert_eq!(st.trial_days_left(now_unix()), TRIAL_DAYS - 5);
        st.first_run = now_unix() - 100 * 86400;
        assert_eq!(st.trial_days_left(now_unix()), 0);
    }

    #[test]
    fn quota_rolling_window_resets_after_24h() {
        let now = 1_000_000u64;
        let mut st = LicenseState::default();
        assert_eq!(st.used_and_bump("gmail", now), 1);
        assert_eq!(st.used_and_bump("gmail", now + 60), 2);
        assert_eq!(st.used_current("gmail", now + 60), 2);
        // Окно ушло — счётчик обнулился.
        assert_eq!(st.used_and_bump("gmail", now + 86400), 1);
        assert_eq!(st.used_current("gmail", now + 86400), 1);
    }

    #[test]
    fn quota_features_are_independent() {
        let now = 1_000_000u64;
        let mut st = LicenseState::default();
        st.used_and_bump(FEATURE_GMAIL, now);
        st.used_and_bump(FEATURE_GMAIL, now);
        assert_eq!(st.used_current(FEATURE_DRIVE, now), 0);
        assert_eq!(st.used_current(FEATURE_GMAIL, now), 2);
    }

    #[test]
    fn state_json_roundtrip() {
        let mut st = LicenseState::default();
        st.first_run = 123;
        st.used_and_bump("nlm", 1000);
        let json = serde_json::to_string(&st).unwrap();
        let back: LicenseState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.first_run, 123);
        assert_eq!(back.used_current("nlm", 1000), 1);
    }

    // civil_date

    #[test]
    fn civil_date_known_values() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(86400), "1970-01-02");
        assert_eq!(civil_date(951_868_800), "2000-03-01");
        assert_eq!(civil_date(1_709_164_800), "2024-02-29"); // високосный день
        assert_eq!(civil_date(1_788_048_000), "2026-08-30");
        assert_eq!(civil_date(2_103_408_000), "2036-08-27");
    }

    // мастер-ключ: целостность встроенного ключа

    #[test]
    fn master_key_hex_is_64_chars_and_parses() {
        assert_eq!(POLER_LICENSE_PUBLIC_KEY_HEX.len(), 64);
        assert!(POLER_LICENSE_PUBLIC_KEY_HEX.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(master_public_key().is_ok());
    }

    fn parse_json(s: &str) -> License {
        serde_json::from_str(s).unwrap()
    }
}
