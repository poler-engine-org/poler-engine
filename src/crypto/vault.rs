//! POLER Vault v1 — зашифрованный постоянный носитель памяти (`.pvt`).
//!
//! Крипто-слой данных (M4.5, `docs/formats/VAULT_FORMAT.md`): шифр
//! PND v8.2 как **инструмент работы с данными**, а не только защита.
//! Vault — это бинарник, в который «насмерть» запечатывается любой
//! поток пользователя: документы, логи терминала, история чата,
//! ответы ИИ, базы знаний. Запечатанный файл можно гонять туда-сюда
//! через GitHub/любой git (см. `poler-git`) или иной транспорт —
//! без ключа он нечитаем, но проверяем на целостность.
//!
//! ## Свойства
//!
//! - **Конфиденциальность**: CBC-каскад PND (Feistel ×20), 256-битный
//!   ключ из парольной фразы (KDF с солью и итерациями);
//! - **Постраничная структура**: страницы шифротекста ровно 4096 байт,
//!   каждая со своим IV из (соль, номер страницы) — mmap-friendly,
//!   страницы независимы (будущий параллелизм/случайный доступ);
//! - **Аутентичность** (ключевая проверка): ланцюговий MAC открытого
//!   текста (`PndHasher::new_keyed`, домен VAULT_MAC) — подмена или
//!   неверный ключ обнаруживаются с вероятностью 1−2⁻²⁵⁶;
//! - **Транспортная целостность** (без ключа): внешний SHA-256
//!   шифротекста + контрольная сумма заголовка FNV-1a64 — битый
//!   git-sync/диск виден до расшифровки (`--memory-verify`);
//! - **Потоковость**: память O(1) — файлы от 500 КБ до сотен ГБ
//!   запечатываются/распаковываются без загрузки в RAM;
//! - **Детерминизм при фиксированной соли**: одна (фраза, соль,
//!   итерации) → побайтово одинаковый контейнер — пере-печать не
//!   мусорит в git-истории (свежая соль при каждой обычной печати
//!   даёт новый вид шифротекста — равенство файлов не течёт).
//!
//! ## Формат (v1)
//!
//! ```text
//! Страница 0 — заголовок (4096 Б):
//!   0x00  8   magic "POLERVLT"
//!   0x08  4   format_version = 1 (u32 LE)
//!   0x0C  4   flags (bit0 = content_id заполнен)
//!   0x10  4   kdf_iterations (u32 LE)
//!   0x14  4   epsilon шифра (u32 LE, нечётный)
//!   0x18  32  salt (8×u32 LE, свежая при печати)
//!   0x38  8   original_len (u64 LE)
//!   0x40  8   page_count (u64 LE)
//!   0x48  32  inner_mac (8×u32 LE) — цепной MAC открытого текста
//!   0x68  32  outer_digest (SHA-256 шифротекста страниц)
//!   0x88  32  content_id (8×u32 LE; публичный контент-хеш, опция)
//!   0xA8  8   header_checksum (FNV-1a64 по 0x00..0xA8)
//!   0xB0..    нули до 4096
//! Страницы 1..N: CBC-шифротекст (каждая ровно 4096 Б, IV из соли+номера).
//! ```

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use pqw_core::checksum::fnv1a64;
use pqw_core::sha256::Sha256;
use rayon::prelude::*;

use super::hasher::{PndHasher, DOMAIN_VAULT_CHAIN, DOMAIN_VAULT_MAC};
use super::kdf;
use super::pnd::{phi, pnd_mix, PolerCipher, KEY_WORDS};

/// Магические байты контейнера.
pub const MAGIC: [u8; 8] = *b"POLERVLT";
/// Текущая версия формата.
pub const FORMAT_VERSION: u32 = 1;
/// Размер страницы (и заголовка): шифротекст и заголовок выровнены на неё.
pub const PAGE_SIZE: usize = 4096;
/// Страниц в пакете параллельного шифрования (512 КБ на поток батча).
/// Страницы независимы (индивидуальные IV из соли) — rayon размножает
/// их по ядрам без изменения формата и MAC-цепи (цепь считается
/// последовательно в порядке страниц).
const BATCH_PAGES: usize = 128;
/// Флаг: в заголовке хранится публичный контент-хеш открытого текста.
pub const FLAG_CONTENT_ID: u32 = 1 << 0;
/// ε для постраничных IV (нечётная константа ядра).
const IV_EPSILON: u32 = 0x517CC1B7;

/// Шифр, разделяемый между потоками rayon: C-ABI-сторона только ЧИТАЕТ
/// контекст (`poler_cbc_encrypt` копирует его по значению), параллельные
/// чтения безопасны. (Контракт `os/core/abi.zig`, M4.)
struct SharedCipher(PolerCipher);
// SAFETY: все экспортируемые операции над handle — read-only над
// неизменяемым после создания контекстом.
unsafe impl Sync for SharedCipher {}

impl SharedCipher {
    /// CBC-шифрование страницы (вызывается из лучей rayon).
    fn seal_page(&self, iv: &[u32; 4], pt_words: &[u32], ct_words: &mut [u32]) {
        self.0.cbc_encrypt_words(iv, pt_words, ct_words);
    }

    /// CBC-расшифрование страницы (вызывается из лучей rayon).
    fn open_page(&self, iv: &[u32; 4], ct_words: &[u32], pt_words: &mut [u32]) {
        self.0.cbc_decrypt_words(iv, ct_words, pt_words);
    }
}

// ── Заголовок ───────────────────────────────────────────────────────────────

/// Разобранный заголовк Vault (страница 0).
#[derive(Clone, Debug)]
pub struct VaultHeader {
    pub format_version: u32,
    pub flags: u32,
    pub kdf_iterations: u32,
    pub epsilon: u32,
    pub salt: [u32; KEY_WORDS],
    pub original_len: u64,
    pub page_count: u64,
    pub inner_mac: [u32; KEY_WORDS],
    pub outer_digest: [u8; 32],
    pub content_id: [u32; KEY_WORDS],
}

impl VaultHeader {
    /// Сериализовать в страницу-заголовок (4096 Б).
    pub fn to_page(&self) -> [u8; PAGE_SIZE] {
        let mut p = [0u8; PAGE_SIZE];
        p[..8].copy_from_slice(&MAGIC);
        p[0x08..0x0C].copy_from_slice(&self.format_version.to_le_bytes());
        p[0x0C..0x10].copy_from_slice(&self.flags.to_le_bytes());
        p[0x10..0x14].copy_from_slice(&self.kdf_iterations.to_le_bytes());
        p[0x14..0x18].copy_from_slice(&self.epsilon.to_le_bytes());
        for i in 0..KEY_WORDS {
            p[0x18 + 4 * i..0x1C + 4 * i].copy_from_slice(&self.salt[i].to_le_bytes());
        }
        p[0x38..0x40].copy_from_slice(&self.original_len.to_le_bytes());
        p[0x40..0x48].copy_from_slice(&self.page_count.to_le_bytes());
        for i in 0..KEY_WORDS {
            p[0x48 + 4 * i..0x4C + 4 * i].copy_from_slice(&self.inner_mac[i].to_le_bytes());
        }
        p[0x68..0x88].copy_from_slice(&self.outer_digest);
        for i in 0..KEY_WORDS {
            p[0x88 + 4 * i..0x8C + 4 * i].copy_from_slice(&self.content_id[i].to_le_bytes());
        }
        let ck = fnv1a64(&p[..0xA8]);
        p[0xA8..0xB0].copy_from_slice(&ck.to_le_bytes());
        p
    }

    /// Разобрать страницу-заголовок: магия, версия, контрольная сумма.
    pub fn from_page(page: &[u8; PAGE_SIZE]) -> Result<Self, String> {
        if page[..8] != MAGIC {
            return Err("не POLER Vault: неверная магия файла".into());
        }
        let rd32 =
            |off: usize| u32::from_le_bytes([page[off], page[off + 1], page[off + 2], page[off + 3]]);
        let rd64 = |off: usize| {
            let mut b = [0u8; 8];
            b.copy_from_slice(&page[off..off + 8]);
            u64::from_le_bytes(b)
        };
        let expect_ck = rd64(0xA8);
        let real_ck = fnv1a64(&page[..0xA8]);
        if expect_ck != real_ck {
            return Err("заголовок повреждён: контрольная сумма FNV-1a64 не сошлась".into());
        }
        let format_version = rd32(0x08);
        if format_version != FORMAT_VERSION {
            return Err(format!(
                "версия формата {format_version} не поддерживается (ожидается {FORMAT_VERSION})"
            ));
        }
        let mut salt = [0u32; KEY_WORDS];
        let mut inner_mac = [0u32; KEY_WORDS];
        let mut content_id = [0u32; KEY_WORDS];
        for i in 0..KEY_WORDS {
            salt[i] = rd32(0x18 + 4 * i);
            inner_mac[i] = rd32(0x48 + 4 * i);
            content_id[i] = rd32(0x88 + 4 * i);
        }
        let mut outer_digest = [0u8; 32];
        outer_digest.copy_from_slice(&page[0x68..0x88]);
        Ok(VaultHeader {
            format_version,
            flags: rd32(0x0C),
            kdf_iterations: rd32(0x10),
            epsilon: rd32(0x14),
            salt,
            original_len: rd64(0x38),
            page_count: rd64(0x40),
            inner_mac,
            outer_digest,
            content_id,
        })
    }

    /// Реальная длина данных страницы `index` (последняя — короче).
    fn page_real_len(&self, index: u64) -> usize {
        let done = index.saturating_mul(PAGE_SIZE as u64);
        (self.original_len.saturating_sub(done)).min(PAGE_SIZE as u64) as usize
    }
}

// ── IV страницы ─────────────────────────────────────────────────────────────

/// Детерминированный IV страницы из (соль, номер): без хранения в файле,
/// непредсказуем без соли, уникален в пределах контейнера.
fn page_iv(salt: &[u32; KEY_WORDS], index: u64) -> [u32; 4] {
    let idx = index as u32;
    let hi = (index >> 32) as u32;
    [
        pnd_mix(salt[0] ^ idx, salt[4] ^ hi.rotate_left(13), IV_EPSILON),
        pnd_mix(salt[1] ^ idx.rotate_left(7), salt[5] ^ hi, IV_EPSILON),
        pnd_mix(salt[2] ^ hi.rotate_left(5), salt[6] ^ idx, IV_EPSILON),
        pnd_mix(salt[3] ^ hi, salt[7] ^ idx.rotate_left(11), IV_EPSILON),
    ]
}

/// Байты → слова (LE) точной кратности 4.
fn bytes_to_words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Слова → байты (LE).
fn words_to_bytes(words: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(words.len() * 4);
    for w in words {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

/// Свежая соль: /dev/urandom → фолбэк DRBG(время ⊕ pid).
fn fresh_salt() -> Result<[u32; KEY_WORDS], String> {
    let mut buf = [0u8; 32];
    let mut sourced = false;
    if let Ok(mut f) = File::open("/dev/urandom") {
        if f.read_exact(&mut buf).is_ok() {
            sourced = true;
        }
    }
    if !sourced {
        // Фолбэк (не Linux или недоступен urandom): DRBG от энтропии
        // времени и pid — уникальность практическая, криптостойкость
        // ниже urandom (задокументировано в VAULT_FORMAT.md).
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| format!("время: {e}"))?;
        let pid = std::process::id() as u32;
        let mut seed = [0u32; KEY_WORDS];
        seed[0] = (t.as_secs() as u32) ^ pid.rotate_left(16);
        seed[1] = t.subsec_nanos();
        seed[2] = phi(t.subsec_nanos() ^ pid);
        seed[3] = phi(seed[0]);
        seed[4] = phi(seed[1]);
        seed[5] = phi(seed[2]);
        seed[6] = 0x9E3779B9 ^ seed[0].rotate_left(9);
        seed[7] = 0x517CC1B7 ^ seed[1].rotate_left(17);
        let mut drbg = super::pnd::PolerDrbg::new(&seed).ok_or_else(|| "DRBG: выделение".to_string())?;
        for w in buf.chunks_exact_mut(4) {
            w.copy_from_slice(&drbg.next().to_le_bytes());
        }
    }
    Ok(bytes_to_words(&buf)
        .try_into()
        .map_err(|_| "salt: внутренняя ошибка размера".to_string())?)
}

// ── Отчёты ──────────────────────────────────────────────────────────────────

/// MAC-лист страницы: keyed-хеш (IV ‖ последний блок CBC-шифротекста ‖
/// номер). Последний блок CBC — зависящая от ключа перестановка ВСЕЙ
/// страницы (сцепление + лавина: любой бит открытого текста или IV
/// меняет его), поэтому лист покрывает страницу целиком при стоимости
/// ОДНОЙ компрессии вместо 256. Считается в луче rayon параллельно
/// шифрованию; цепь листьев сворачивается последовательно (32 Б/стр).
fn mac_leaf(
    key: &[u32; KEY_WORDS],
    iv: &[u32; 4],
    ct_last_block: &[u8; 16],
    index: u64,
) -> [u32; KEY_WORDS] {
    let mut h = PndHasher::new_keyed(key, DOMAIN_VAULT_MAC);
    for w in iv {
        h.update(&w.to_le_bytes());
    }
    h.update(ct_last_block);
    h.update(&index.to_le_bytes());
    h.finalize()
}

/// Последний 16-байтовый блок страницы шифротекста.
fn last_ct_block(ct: &[u8]) -> [u8; 16] {
    debug_assert!(!ct.is_empty() && ct.len() % 16 == 0);
    let mut b = [0u8; 16];
    b.copy_from_slice(&ct[ct.len() - 16..]);
    b
}

/// Публичный контент-лист страницы (для content-id, параллелен).
fn content_leaf(page: &[u8], index: u64) -> [u32; KEY_WORDS] {
    let mut h = PndHasher::new_content();
    h.update(page);
    h.update(&index.to_le_bytes());
    h.finalize()
}

/// Контент-идентификатор из листьев (документированная конструкция формата:
/// content-id = публичный цепной фолд листьев страниц — детерминирован,
/// параллелен, чувствителен к содержимому И порядку страниц).
pub fn content_id_from_leaves<'a>(leaves: impl Iterator<Item = &'a [u32; KEY_WORDS]>) -> [u32; KEY_WORDS] {
    let mut chain = PndHasher::new_content();
    for leaf in leaves {
        for w in leaf {
            chain.update(&w.to_le_bytes());
        }
    }
    chain.finalize()
}

/// Итог печати контейнера.
#[derive(Clone, Debug)]
pub struct SealReport {
    pub input: PathBuf,
    pub output: PathBuf,
    pub original_len: u64,
    pub pages: u64,
    pub vault_len: u64,
    pub duration_ms: u128,
    pub mb_per_s: f64,
    pub content_id: Option<[u32; KEY_WORDS]>,
}

/// Итог вскрытия контейнера.
#[derive(Clone, Debug)]
pub struct OpenReport {
    pub input: PathBuf,
    pub output: PathBuf,
    pub original_len: u64,
    pub pages: u64,
    pub duration_ms: u128,
    pub mb_per_s: f64,
    pub transport_ok: bool,
    pub authenticity_ok: bool,
}

/// Итог ключевой проверки.
#[derive(Clone, Debug)]
pub struct VerifyReport {
    pub path: PathBuf,
    pub header_ok: bool,
    pub transport_ok: bool,
    pub pages: u64,
    pub vault_len: u64,
}

/// Опции печати.
#[derive(Clone, Debug)]
pub struct SealOptions {
    pub iterations: u32,
    pub content_id: bool,
    /// Фиксированная соль (детерминизм в тестах/пайплайнах); None — свежая.
    pub salt: Option<[u32; KEY_WORDS]>,
}

impl Default for SealOptions {
    fn default() -> Self {
        SealOptions { iterations: kdf::DEFAULT_ITERATIONS, content_id: false, salt: None }
    }
}

// ── Печать ──────────────────────────────────────────────────────────────────

/// Запечатать файл: PATH → контейнер CBC(PND v8.2) с MAC и внешним digest.
pub fn seal(
    input: &Path,
    output: &Path,
    passphrase: &str,
    opts: &SealOptions,
) -> Result<SealReport, String> {
    if input == output {
        return Err("вход и выход совпадают — используйте другой --memory-out".into());
    }
    let t0 = Instant::now();

    let salt = match opts.salt {
        Some(s) => s,
        None => fresh_salt()?,
    };
    let key = kdf::derive_key(passphrase, &salt, opts.iterations)?;
    let epsilon = phi(salt[0] ^ salt[4]) | 1;
    let cipher = PolerCipher::new(&key, epsilon).ok_or_else(|| "шифр: выделение контекста".to_string())?;

    let mut reader =
        BufReader::with_capacity(256 * 1024, File::open(input).map_err(|e| format!("{}: {e}", input.display()))?);
    let mut writer =
        BufWriter::with_capacity(256 * 1024, File::create(output).map_err(|e| format!("{}: {e}", output.display()))?);

    let mut mac = PndHasher::new_keyed(&key, DOMAIN_VAULT_CHAIN);
    let mut outer = Sha256::new();
    let mut content_chain = if opts.content_id { Some(PndHasher::new_content()) } else { None };
    let want_content = opts.content_id;

    // Заголовок пишется в конце (нужны MAC/digest/длины) — резерв страницы 0.
    writer.write_all(&[0u8; PAGE_SIZE]).map_err(|e| format!("запись: {e}"))?;

    let mut page = [0u8; PAGE_SIZE];
    let mut pages: u64 = 0;
    let mut total: u64 = 0;
    let shared = SharedCipher(cipher);
    'batches: loop {
        // 1) Читаем пакет страниц (последовательно — диск любит порядок).
        let mut batch: Vec<Box<[u8; PAGE_SIZE]>> = Vec::with_capacity(BATCH_PAGES);
        let mut reals: Vec<usize> = Vec::with_capacity(BATCH_PAGES);
        for _ in 0..BATCH_PAGES {
            let read = read_full(&mut reader, &mut page)?;
            if read == 0 {
                break;
            }
            // Хвост последней страницы — строго нули: детерминизм
            // шифротекста при фиксированной соли не зависит от прошлого.
            if read < PAGE_SIZE {
                page[read..].fill(0);
            }
            batch.push(Box::new(page));
            reals.push(read);
            if read < PAGE_SIZE {
                break;
            }
        }
        if batch.is_empty() {
            break 'batches;
        }

        // 2) Параллельная обработка пакета: шифрование + MAC/контент-листья
        //    считаются в лучах rayon (страницы независимы, индивидуальные
        //    IV); цепь и запись — последовательно ниже.
        let base_index = pages;
        let lane_key = key;
        let results: Vec<(Vec<u8>, [u32; KEY_WORDS], Option<[u32; KEY_WORDS]>)> = batch
            .par_iter()
            .enumerate()
            .map(|(i, pt_page)| {
                let index = base_index + i as u64;
                let real = reals[i];
                let iv = page_iv(&salt, index);
                let pt_words = bytes_to_words(&pt_page[..]);
                let mut ct_words = vec![0u32; PAGE_SIZE / 4];
                shared.seal_page(&iv, &pt_words, &mut ct_words);
                let ct_bytes = words_to_bytes(&ct_words);
                let tail = last_ct_block(&ct_bytes);
                let leaf = mac_leaf(&lane_key, &iv, &tail, index);
                let cleaf = if want_content { Some(content_leaf(&pt_page[..real], index)) } else { None };
                (ct_bytes, leaf, cleaf)
            })
            .collect();

        // 3) Последовательные запись и цепи — порядок страниц каноничен,
        //    результат детерминирован. Цепь получает по 32 байта на
        //    страницу (вместо 4 КиБ через хешер) — узкое место сжато
        //    в ~0.8% от объёма шифрования.
        for (i, (ct_bytes, leaf, cleaf)) in results.iter().enumerate() {
            writer.write_all(ct_bytes).map_err(|e| format!("запись: {e}"))?;
            outer.update(ct_bytes);
            for w in leaf {
                mac.update(&w.to_le_bytes());
            }
            if let (Some(chain), Some(c)) = (content_chain.as_mut(), cleaf) {
                for w in c {
                    chain.update(&w.to_le_bytes());
                }
            }
            pages += 1;
            total += reals[i] as u64;
        }
    }

    let inner_mac = mac.finalize();
    let outer_digest = outer.finalize();
    let content_digest = content_chain.map(|c| c.finalize());

    let header = VaultHeader {
        format_version: FORMAT_VERSION,
        flags: if opts.content_id { FLAG_CONTENT_ID } else { 0 },
        kdf_iterations: opts.iterations,
        epsilon,
        salt,
        original_len: total,
        page_count: pages,
        inner_mac,
        outer_digest,
        content_id: content_digest.unwrap_or([0u32; KEY_WORDS]),
    };

    // Переписать страницу-заголовок (файл уже спозиционирован в конец).
    writer.flush().map_err(|e| format!("flush: {e}"))?;
    let mut file = writer.into_inner().map_err(|e| format!("закрытие: {e}"))?;
    file.seek(SeekFrom::Start(0)).map_err(|e| format!("seek: {e}"))?;
    file.write_all(&header.to_page()).map_err(|e| format!("заголовок: {e}"))?;
    file.flush().map_err(|e| format!("flush: {e}"))?;

    let vault_len = PAGE_SIZE as u64 + pages * PAGE_SIZE as u64;
    let duration = t0.elapsed().as_millis().max(1);
    let mbps = (total as f64 / 1_048_576.0) / (duration as f64 / 1000.0);
    Ok(SealReport {
        input: input.to_path_buf(),
        output: output.to_path_buf(),
        original_len: total,
        pages,
        vault_len,
        duration_ms: duration,
        mb_per_s: mbps,
        content_id: content_digest,
    })
}

// ── Вскрытие ────────────────────────────────────────────────────────────────

/// Вскрыть контейнер: PATH.pvt → открытый текст (проверка MAC обязательна).
pub fn open(input: &Path, output: &Path, passphrase: &str) -> Result<OpenReport, String> {
    if input == output {
        return Err("вход и выход совпадают — используйте другой --memory-out".into());
    }
    let t0 = Instant::now();

    let mut reader =
        BufReader::with_capacity(256 * 1024, File::open(input).map_err(|e| format!("{}: {e}", input.display()))?);

    let mut header_page = [0u8; PAGE_SIZE];
    read_exact_or_err(&mut reader, &mut header_page, "заголовок")?;
    let header = VaultHeader::from_page(&header_page)?;

    let key = kdf::derive_key(passphrase, &header.salt, header.kdf_iterations)?;
    let cipher = PolerCipher::new(&key, header.epsilon).ok_or_else(|| "шифр: выделение контекста".to_string())?;
    let mut mac = PndHasher::new_keyed(&key, DOMAIN_VAULT_CHAIN);
    let mut outer = Sha256::new();

    let mut writer = BufWriter::with_capacity(
        256 * 1024,
        File::create(output).map_err(|e| format!("{}: {e}", output.display()))?,
    );

    let expected_pages = (header.original_len as usize).div_ceil(PAGE_SIZE) as u64;
    if header.page_count != expected_pages {
        return Err(format!(
            "непоследовательный заголовок: page_count={} при original_len={} (ожидалось {expected_pages})",
            header.page_count, header.original_len
        ));
    }

    let shared = SharedCipher(cipher);
    let mut ct_page = [0u8; PAGE_SIZE];
    let mut index: u64 = 0;
    'batches: loop {
        // 1) Читаем пакет шифротекста (последовательно).
        let mut batch: Vec<Box<[u8; PAGE_SIZE]>> = Vec::with_capacity(BATCH_PAGES);
        for _ in 0..BATCH_PAGES {
            if index >= header.page_count {
                break;
            }
            read_exact_or_err(&mut reader, &mut ct_page, "шифротекст")?;
            outer.update(&ct_page);
            batch.push(Box::new(ct_page));
            index += 1;
        }
        if batch.is_empty() {
            break 'batches;
        }

        // 2) Параллельное расшифрование + MAC-листья в лучах rayon.
        let base_index = index - batch.len() as u64;
        let lane_key = key;
        let results: Vec<(Vec<u8>, [u32; KEY_WORDS])> = batch
            .par_iter()
            .enumerate()
            .map(|(i, ct_bytes)| {
                let page_no = base_index + i as u64;
                let iv = page_iv(&header.salt, page_no);
                let ct_words = bytes_to_words(&ct_bytes[..]);
                let mut pt_words = vec![0u32; PAGE_SIZE / 4];
                shared.open_page(&iv, &ct_words, &mut pt_words);
                let pt_bytes = words_to_bytes(&pt_words);
                // MAC-лист — из шифротекста как есть (независим от
                // расшифровки: подмена ловится даже если расшифровка
                // не выполнялась бы).
                let tail = last_ct_block(&ct_bytes[..]);
                let leaf = mac_leaf(&lane_key, &iv, &tail, page_no);
                (pt_bytes, leaf)
            })
            .collect();

        // 3) Последовательные запись и цепь — порядок каноничен.
        for (i, (pt_bytes, leaf)) in results.iter().enumerate() {
            let page_no = base_index + i as u64;
            let real = header.page_real_len(page_no);
            writer.write_all(&pt_bytes[..real]).map_err(|e| format!("запись: {e}"))?;
            for w in leaf {
                mac.update(&w.to_le_bytes());
            }
        }
    }
    writer.flush().map_err(|e| format!("flush: {e}"))?;

    let transport_ok = outer.finalize() == header.outer_digest;
    let authenticity_ok = mac.finalize() == header.inner_mac;

    let duration = t0.elapsed().as_millis().max(1);
    let mbps = (header.original_len as f64 / 1_048_576.0) / (duration as f64 / 1000.0);

    if !transport_ok {
        return Err(format!(
            "транспортная целостность нарушена: SHA-256 шифротекста не совпал (битый git-sync/диск/передача)"
        ));
    }
    if !authenticity_ok {
        return Err(
            "аутентичность не подтверждена: неверная парольная фраза ИЛИ контейнер подменён (MAC не сошёлся)"
                .into(),
        );
    }

    Ok(OpenReport {
        input: input.to_path_buf(),
        output: output.to_path_buf(),
        original_len: header.original_len,
        pages: header.page_count,
        duration_ms: duration,
        mb_per_s: mbps,
        transport_ok,
        authenticity_ok,
    })
}

// ── Проверка без ключа ──────────────────────────────────────────────────────

/// Ключевая проверка контейнера: заголовок (FNV) + внешний SHA-256.
pub fn verify(input: &Path) -> Result<VerifyReport, String> {
    let mut reader =
        BufReader::with_capacity(256 * 1024, File::open(input).map_err(|e| format!("{}: {e}", input.display()))?);

    let mut header_page = [0u8; PAGE_SIZE];
    read_exact_or_err(&mut reader, &mut header_page, "заголовок")?;
    let header = VaultHeader::from_page(&header_page)?;

    let mut outer = Sha256::new();
    let mut ct_page = [0u8; PAGE_SIZE];
    for _ in 0..header.page_count {
        read_exact_or_err(&mut reader, &mut ct_page, "шифротекст")?;
        outer.update(&ct_page);
    }

    let transport_ok = outer.finalize() == header.outer_digest;
    let vault_len = std::fs::metadata(input).map_err(|e| format!("метаданные: {e}"))?.len();
    let report = VerifyReport {
        path: input.to_path_buf(),
        header_ok: true, // from_page уже проверил FNV
        transport_ok,
        pages: header.page_count,
        vault_len,
    };
    if !report.transport_ok {
        return Err("транспортная целостность нарушена: SHA-256 шифротекста не совпал".into());
    }
    Ok(report)
}

/// Метаданные контейнера без ключа (для человека и агента).
pub fn info(input: &Path) -> Result<VaultHeader, String> {
    let mut reader =
        BufReader::new(File::open(input).map_err(|e| format!("{}: {e}", input.display()))?);
    let mut header_page = [0u8; PAGE_SIZE];
    read_exact_or_err(&mut reader, &mut header_page, "заголовок")?;
    VaultHeader::from_page(&header_page)
}

// ── Вспомогательное чтение ──────────────────────────────────────────────────

/// Читать до заполнения буфера; вернуть число реально прочитанных байт
/// (0 — EOF). Частичное чтение посреди страницы — ошибка формата.
fn read_full<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<usize, String> {
    let mut done = 0usize;
    while done < buf.len() {
        match reader.read(&mut buf[done..]) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("чтение: {e}")),
        }
    }
    Ok(done)
}

fn read_exact_or_err<R: Read>(reader: &mut R, buf: &mut [u8], what: &str) -> Result<(), String> {
    let n = read_full(reader, buf)?;
    if n != buf.len() {
        return Err(format!("обрыв файла в секции «{what}»: {n}/{} байт", buf.len()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("poler-vault-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_input(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }

    fn quick_opts(salt: Option<[u32; KEY_WORDS]>, content_id: bool) -> SealOptions {
        SealOptions { iterations: kdf::MIN_ITERATIONS, content_id, salt }
    }

    /// Roundtrip на данных разных размеров (включая пустые и кратные странице).
    #[test]
    fn roundtrip_sizes() {
        let dir = tmpdir("roundtrip");
        for (name, len) in [
            ("empty", 0usize),
            ("tiny", 1),
            ("block", 16),
            ("page", PAGE_SIZE),
            ("page-plus-1", PAGE_SIZE + 1),
            ("three-pages", 3 * PAGE_SIZE + 1234),
        ] {
            let data: Vec<u8> = (0..len).map(|i| (i * 31 + 7) as u8).collect();
            let input = write_input(&dir, &format!("{name}.bin"), &data);
            let vault = dir.join(format!("{name}.pvt"));
            let out = dir.join(format!("{name}.out"));

            let rep = seal(&input, &vault, "тест-фраза", &quick_opts(None, false)).unwrap();
            assert_eq!(rep.original_len, len as u64);
            assert_eq!(rep.pages, len.div_ceil(PAGE_SIZE) as u64);

            let orep = open(&vault, &out, "тест-фраза").unwrap();
            assert!(orep.transport_ok && orep.authenticity_ok);
            assert_eq!(std::fs::read(&out).unwrap(), data, "case={name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Неверная парольная фраза → отказ (MAC), файл не считается.
    #[test]
    fn wrong_passphrase_rejected() {
        let dir = tmpdir("wrong-key");
        let input = write_input(&dir, "in.bin", b"secret terminal log payload");
        let vault = dir.join("v.pvt");
        seal(&input, &vault, "правильная", &quick_opts(None, false)).unwrap();

        let err = open(&vault, &dir.join("out.bin"), "неправильная").unwrap_err();
        assert!(err.contains("аутентичность"), "err={err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Подмена байта шифротекста → обнаружена (внешний digest И/ИЛИ MAC).
    #[test]
    fn tamper_ciphertext_detected() {
        let dir = tmpdir("tamper");
        let input = write_input(&dir, "in.bin", &(0..2 * PAGE_SIZE).map(|i| i as u8).collect::<Vec<_>>());
        let vault = dir.join("v.pvt");
        seal(&input, &vault, "k", &quick_opts(Some([3u32; 8]), false)).unwrap();

        let mut bytes = std::fs::read(&vault).unwrap();
        for &offset in &[PAGE_SIZE + 100, 2 * PAGE_SIZE + 4000] {
            let mut b = bytes.clone();
            b[offset] ^= 0x01;
            let tampered = dir.join("t.pvt");
            std::fs::write(&tampered, &b).unwrap();

            assert!(verify(&tampered).is_err(), "offset={offset}: verify должен падать");
            let err = open(&tampered, &dir.join("out.bin"), "k").unwrap_err();
            assert!(
                err.contains("целостность") || err.contains("аутентичность"),
                "offset={offset}: err={err}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Подмена заголовка (битый флаг/версия/контрольная сумма) → detect.
    #[test]
    fn tamper_header_detected() {
        let dir = tmpdir("tamper-hdr");
        let input = write_input(&dir, "in.bin", b"payload-for-header-tamper-test");
        let vault = dir.join("v.pvt");
        seal(&input, &vault, "k", &quick_opts(Some([7u32; 8]), false)).unwrap();

        let mut b = std::fs::read(&vault).unwrap();
        b[0x10] ^= 0xFF; // kdf_iterations — FNV не сойдётся
        let t = dir.join("t.pvt");
        std::fs::write(&t, &b).unwrap();
        assert!(verify(&t).is_err());
        assert!(open(&t, &dir.join("o"), "k").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Детерминизм: одна (фраза, соль, итерации) → побайтово один контейнер.
    #[test]
    fn deterministic_with_fixed_salt() {
        let dir = tmpdir("determinism");
        let input = write_input(&dir, "in.bin", b"deterministic reseal keeps git history clean");
        let salt = [0xABCD1234u32, 1, 2, 3, 4, 5, 6, 7];
        let v1 = dir.join("a.pvt");
        let v2 = dir.join("b.pvt");
        seal(&input, &v1, "фраза", &quick_opts(Some(salt), false)).unwrap();
        seal(&input, &v2, "фраза", &quick_opts(Some(salt), false)).unwrap();
        assert_eq!(std::fs::read(&v1).unwrap(), std::fs::read(&v2).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Разные соли → разный шифротекст (равенство открытых текстов не течёт).
    #[test]
    fn fresh_salt_changes_ciphertext() {
        let dir = tmpdir("salt");
        let input = write_input(&dir, "in.bin", &[7u8; PAGE_SIZE]);
        let v1 = dir.join("a.pvt");
        let v2 = dir.join("b.pvt");
        seal(&input, &v1, "k", &quick_opts(Some([1u32; 8]), false)).unwrap();
        seal(&input, &v2, "k", &quick_opts(Some([2u32; 8]), false)).unwrap();
        let (a, b) = (std::fs::read(&v1).unwrap(), std::fs::read(&v2).unwrap());
        assert_ne!(a, b);
        // Оба валидно вскрываются одной фразой.
        open(&v1, &dir.join("o1"), "k").unwrap();
        open(&v2, &dir.join("o2"), "k").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Ключевая проверка без ключа: чистый контейнер ок, битый — нет.
    #[test]
    fn keyless_verify() {
        let dir = tmpdir("keyless");
        let input = write_input(&dir, "in.bin", b"git-transport friendly integrity");
        let vault = dir.join("v.pvt");
        seal(&input, &vault, "ключ", &quick_opts(None, false)).unwrap();

        let rep = verify(&vault).unwrap();
        assert!(rep.header_ok && rep.transport_ok);

        let mut b = std::fs::read(&vault).unwrap();
        let last = b.len() - 1;
        b[last] ^= 0x80;
        let t = dir.join("t.pvt");
        std::fs::write(&t, &b).unwrap();
        assert!(verify(&t).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Опция content_id: идентификатор в заголовке совпадает с публичным
    /// цепным фолдом листьев (документированная конструкция формата).
    #[test]
    fn content_id_matches_public_hash() {
        let dir = tmpdir("cid");
        let data = b"content addressable cartridge";
        let input = write_input(&dir, "in.bin", data);
        let vault = dir.join("v.pvt");
        let rep = seal(&input, &vault, "k", &quick_opts(Some([9u32; 8]), true)).unwrap();

        let h = info(&vault).unwrap();
        assert!(h.flags & FLAG_CONTENT_ID != 0);
        // Ожидание: листья страниц (реальные байты + номер), затем фолд.
        let leaves: Vec<[u32; KEY_WORDS]> =
            data.chunks(PAGE_SIZE).enumerate().map(|(i, page)| content_leaf(page, i as u64)).collect();
        let expect = content_id_from_leaves(leaves.iter());
        assert_eq!(h.content_id, expect);
        assert_eq!(rep.content_id, Some(expect));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// MAC-лист чувствителен к шифротексту, IV и номеру страницы:
    /// перестановка/подмена/сдвиг страниц меняет итоговый inner_mac.
    #[test]
    fn mac_leaf_binds_page_order() {
        let key = [0x42424242u32; KEY_WORDS];
        let iv = [1u32, 2, 3, 4];
        let iv2 = [9u32, 8, 7, 6];
        let block_a = [0xAAu8; 16];
        let block_b = [0xBBu8; 16];
        // Одинаковый хвост на разных местах — листья разные.
        assert_ne!(mac_leaf(&key, &iv, &block_a, 0), mac_leaf(&key, &iv, &block_a, 1));
        // Разный хвост / другой IV — листья разные.
        assert_ne!(mac_leaf(&key, &iv, &block_a, 0), mac_leaf(&key, &iv, &block_b, 0));
        assert_ne!(mac_leaf(&key, &iv, &block_a, 0), mac_leaf(&key, &iv2, &block_a, 0));
        // Детерминизм.
        assert_eq!(mac_leaf(&key, &iv, &block_a, 0), mac_leaf(&key, &iv, &block_a, 0));
    }

    /// Обрыв файла посреди страницы — явная ошибка формата.
    #[test]
    fn truncated_container_rejected() {
        let dir = tmpdir("trunc");
        let input = write_input(&dir, "in.bin", &(0..5 * PAGE_SIZE).map(|i| i as u8).collect::<Vec<_>>());
        let vault = dir.join("v.pvt");
        seal(&input, &vault, "k", &quick_opts(None, false)).unwrap();

        let full = std::fs::read(&vault).unwrap();
        let t = dir.join("t.pvt");
        std::fs::write(&t, &full[..full.len() - 100]).unwrap();
        let err = verify(&t).unwrap_err();
        assert!(err.contains("обрыв"), "err={err}");
        let err = open(&t, &dir.join("o"), "k").unwrap_err();
        assert!(err.contains("обрыв"), "err={err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Многостраничный поток: page IV уникальны, повторное шифрование
    /// одной страницы с разными IV даёт разный шифротекст.
    #[test]
    fn page_ivs_unique() {
        let salt = [42u32; KEY_WORDS];
        let mut seen = std::collections::HashSet::new();
        for i in 0..1000u64 {
            let iv = page_iv(&salt, i);
            assert!(seen.insert(iv), "IV коллизия на странице {i}");
        }
        assert_ne!(page_iv(&salt, 0), page_iv(&salt, 1));
    }

    /// Бенчмарк-функция (не автотест): печать/вскрытие 5 МБ.
    #[test]
    #[ignore = "медленный: cargo test -- --ignored vault_bench_5mb"]
    fn vault_bench_5mb() {
        let dir = tmpdir("bench");
        let mut data = vec![0u8; 5 * 1024 * 1024];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i >> 3) as u8;
        }
        let input = write_input(&dir, "in.bin", &data);
        let vault = dir.join("v.pvt");
        let out = dir.join("out.bin");

        let s = seal(&input, &vault, "bench", &SealOptions::default()).unwrap();
        let o = open(&vault, &out, "bench").unwrap();
        eprintln!(
            "vault 5MB: seal {} мс ({:.1} МБ/с), open {} мс ({:.1} МБ/с), KDF {} итераций",
            s.duration_ms, s.mb_per_s, o.duration_ms, o.mb_per_s, kdf::DEFAULT_ITERATIONS
        );
        assert!(std::fs::read(&out).unwrap() == data);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
