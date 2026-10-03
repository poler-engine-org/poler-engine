//! Потоковый инжест контейнеров (v0.62.0, IngestPipeline).
//!
//! Автоматическая распаковка на лету входных архивов (.zip, .tar.gz, .tar.zst, .gz, .zst)
//! с детекцией формата по сигнатурам (magic bytes). Распакованный поток данных льется
//! напрямую в FastCDC + BLAKE3 + ZSTD StreamWriter без промежуточного сохранения на диск.
//!
//! v0.65.0 (ансамбль):
//! * [`ingest_reader`] — тот же конвейер для ЛЮБОГО байтового источника
//!   (HTTP-стрим `--stream-download`, stdin): сниффинг магии первых ≤512 байт,
//!   декодирование gzip/zstd НА ЛЕТУ, границы tar-файлов — TarObserver'ом
//!   писателя. Сетевые исходники ложатся в .poler сразу с пофайловой
//!   таблицей и zstd-сжатием — сырой tar.gz на диск НЕ пишется вовсе.
//! * [`pack_dir`] — «постоянный архиватор сборки»: упаковка каталога
//!   в .poler с пофайловой таблицей (права 0o755 сохраняются — poler-box
//!   запускает бинарники прямо из архива).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use super::stream_writer::{StreamWriter, StreamWriteConfig, StreamWriteStats};

/// Режим инжеста входного потока.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum IngestMode {
    /// Автодетекция по сигнатурам (распаковка на лету при обнаружении контейнера).
    #[default]
    Auto,
    /// Прямой слепой поток (без распаковки).
    Off,
}


/// Распознанный внутренний формат входного потока/файла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestKind {
    Zip,
    TarGz,
    TarZst,
    GzSingle,
    ZstSingle,
    TarRaw,
    RawStream,
}

/// Детекция формата по сигнатуре первых байт.
pub fn sniff_magic(magic: &[u8]) -> IngestKind {
    if magic.len() >= 4 && &magic[0..4] == b"PK\x03\x04" {
        return IngestKind::Zip;
    }
    if magic.len() >= 2 && magic[0] == 0x1f && magic[1] == 0x8b {
        return IngestKind::TarGz;
    }
    if magic.len() >= 4 && magic[0..4] == [0x28, 0xb5, 0x2f, 0xfd] {
        return IngestKind::TarZst;
    }
    if magic.len() >= 262 && &magic[257..262] == b"ustar" {
        return IngestKind::TarRaw;
    }
    IngestKind::RawStream
}

/// Потоковая упаковка файла в .poler с авто-распаковкой на лету.
pub fn ingest_file(
    src_path: &Path,
    out_path: &Path,
    cfg: StreamWriteConfig,
    mode: IngestMode,
) -> io::Result<StreamWriteStats> {
    let mut f = File::open(src_path)?;
    let file_len = f.metadata()?.len();

    if mode == IngestMode::Off || file_len < 4 {
        return super::stream_writer::write_stream(&mut f, out_path, cfg, &src_path.to_string_lossy());
    }

    let mut header = [0u8; 512];
    let n = f.read(&mut header)?;
    f.seek(SeekFrom::Start(0))?;

    match sniff_magic(&header[..n]) {
        // zip требует seek — только локальный файл.
        IngestKind::Zip => {
            let mut writer = StreamWriter::open(out_path, cfg)?;
            let mut archive = zip::ZipArchive::new(f)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("zip: {e}")))?;

            // Вливаем entries в tar-совместимый поток для сохранения файловой таблицы
            {
                let mut tar_builder = tar::Builder::new(TarWriterShim { writer: &mut writer });
                for i in 0..archive.len() {
                    let mut file = archive.by_index(i)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("zip entry: {e}")))?;
                    if file.is_dir() {
                        continue;
                    }
                    let mut header = tar::Header::new_gnu();
                    header.set_size(file.size());
                    header.set_mode(0o644);
                    header.set_cksum();
                    let name = file.name().to_string();
                    tar_builder.append_data(&mut header, name, &mut file)?;
                }
                tar_builder.finish()?;
            }
            let hint = src_path.file_name().unwrap_or_default().to_string_lossy().to_string();
            writer.finish(&hint)
        }
        // tar.gz / tar.zst / gz / zst / raw tar / сырой поток — стриминговый путь.
        _ => {
            let hint = src_path.file_name().unwrap_or_default().to_string_lossy().to_string();
            ingest_reader(f, out_path, cfg, mode, &hint)
        }
    }
}

/// Потоковый инжест ЛЮБОГО байтового источника (v0.65.0): HTTP-стрим
/// `--stream-download`, stdin, сокет. Сниффинг магии первых ≤512 байт →
/// декодер gzip/zstd НА ЛЕТУ → StreamWriter (TarObserver сам разводит
/// пофайловую таблицу tar-потока). zip по потоку НЕ распаковывается
/// (требует seek) — сохраняется как есть одним entry.
///
/// RAM-дисциплина: буферы 256 КиБ, MultiGzDecoder/Decoder держат
/// только свои окна; сырые байты на диск не попадают НИКОГДА.
pub fn ingest_reader<R: Read>(
    src: R,
    out_path: &Path,
    cfg: StreamWriteConfig,
    mode: IngestMode,
    name_hint: &str,
) -> io::Result<StreamWriteStats> {
    let mut src = src;
    let mut head = [0u8; 512];
    let mut n = 0usize;
    while n < head.len() {
        let r = src.read(&mut head[n..])?;
        if r == 0 {
            break;
        }
        n += r;
    }
    let kind = if mode == IngestMode::Off || n < 4 {
        IngestKind::RawStream
    } else {
        sniff_magic(&head[..n])
    };
    let head_vec = head[..n].to_vec();

    match kind {
        IngestKind::TarGz | IngestKind::GzSingle => {
            let mut writer = StreamWriter::open(out_path, cfg)?;
            let chained = io::Cursor::new(head_vec).chain(src);
            let gz = flate2::read::MultiGzDecoder::new(chained);
            let mut reader = std::io::BufReader::with_capacity(256 * 1024, gz);
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                writer.push_bytes(&buf[..n])?;
            }
            writer.finish(name_hint)
        }
        IngestKind::TarZst | IngestKind::ZstSingle => {
            let mut writer = StreamWriter::open(out_path, cfg)?;
            let chained = io::Cursor::new(head_vec).chain(src);
            let zst = zstd::stream::read::Decoder::new(chained)?;
            let mut reader = std::io::BufReader::with_capacity(256 * 1024, zst);
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                writer.push_bytes(&buf[..n])?;
            }
            writer.finish(name_hint)
        }
        // Raw tar / сырой поток / zip-по-стриму: TarObserver сам определяет
        // tar; не-tar поток станет одиночной записью name_hint.
        _ => {
            let mut writer = StreamWriter::open(out_path, cfg)?;
            writer.push_bytes(&head_vec)?;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = src.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                writer.push_bytes(&buf[..n])?;
            }
            writer.finish(name_hint)
        }
    }
}

/// «Постоянный архиватор сборки» (v0.65.0): упаковка каталога в .poler
/// с ПОФАЙЛОВОЙ таблицей. Обход как у grep-слоя (respect .gitignore,
/// скрытые по умолчанию пропускаются); файлы льются через tar-обёртку —
/// TarObserver писателя разводит границы файлов, SHA256 каждой записи
/// и BLAKE3-дедуп МЕЖДУ файлами достаются бесплатно. Права (0o755)
/// сохраняются в tar-заголовках — poler-box запускает бинарники прямо
/// из архива (циклический ансамбль: движок в коробке из своего .poler).
pub fn pack_dir(
    root: &Path,
    out_path: &Path,
    cfg: StreamWriteConfig,
    include_hidden: bool,
    respect_ignore: bool,
) -> io::Result<StreamWriteStats> {
    use ignore::WalkBuilder;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    // Грабля Сессии-12: выходной архив, писавшийся ВНУТРИ упаковываемого
    // каталога, попадал в обход — пакуется растущий сам-себя .part и
    // sidecar таблицы, реальные файлы теряются молча. Пропускаем вывод
    // и его sidecar-файлы (out, out.part, out.part.files, out.files).
    let self_outputs: Vec<std::path::PathBuf> = {
        let abs = |p: &Path| -> std::path::PathBuf {
            if let Ok(c) = std::fs::canonicalize(p) {
                return c;
            }
            // Файла ещё нет (например, финальный .poler до rename) —
            // лексическая нормализация от текущего каталога.
            let joined = if p.is_absolute() {
                p.to_path_buf()
            } else {
                std::env::current_dir().unwrap_or_default().join(p)
            };
            let mut pb = std::path::PathBuf::new();
            for c in joined.components() {
                match c {
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        pb.pop();
                    }
                    other => {
                        pb.push(other.as_os_str());
                    }
                }
            }
            pb
        };
        let mut suffixes = vec![std::path::PathBuf::new()];
        suffixes.push(".part".into());
        suffixes.push(".part.files".into());
        suffixes.push(".files".into());
        let mut v = Vec::new();
        for sfx in &suffixes {
            let mut p = out_path.as_os_str().to_os_string();
            p.push(sfx.as_os_str());
            v.push(abs(Path::new(&p)));
        }
        v
    };

    let mut writer = StreamWriter::open(out_path, cfg)?;
    {
        let mut tar_builder = tar::Builder::new(TarWriterShim { writer: &mut writer });
        let walker = WalkBuilder::new(root)
            .hidden(!include_hidden)
            .git_ignore(respect_ignore)
            .git_global(respect_ignore)
            .git_exclude(respect_ignore)
            .ignore(respect_ignore)
            .require_git(false)
            .build();
        for entry in walker.filter_map(|e| e.ok()) {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = entry.path();
            // Свой вывод не пакуем (self-reference — см. граблю выше).
            let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            if self_outputs.iter().any(|p| *p == canon) {
                continue;
            }
            let rel = path.strip_prefix(root).unwrap_or(path);
            let name = rel.to_string_lossy().replace('\\', "/");
            let meta = std::fs::metadata(path)?;
            let mut f = File::open(path)?;
            let mut header = tar::Header::new_gnu();
            header.set_size(meta.len());
            header.set_mode(meta.permissions().mode() & 0o7777);
            header.set_cksum();
            tar_builder.append_data(&mut header, name, &mut f)?;
        }
        tar_builder.finish()?;
    }
    let hint = root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    writer.finish(&hint)
}

struct TarWriterShim<'a> {
    writer: &'a mut StreamWriter,
}

impl<'a> io::Write for TarWriterShim<'a> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.push_bytes(buf)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// ────────────────────────── тесты (v0.65.0 ансамбль) ──────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::reader::PolerReader;
    use std::io::Write;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("poler-ing-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    fn cfg() -> StreamWriteConfig {
        StreamWriteConfig { progress_bytes: 0, ..Default::default() }
    }

    /// tar.gz в памяти: три файла, вложенные каталоги. Возвращает
    /// (gzip-байты, размер РАСПАКОВАННОГО tar) — экономия диска
    /// меряется против распакованного, а не против gz.
    fn tar_gz_bytes() -> (Vec<u8>, usize) {
        let mut tar_bytes = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tar_bytes);
            b.append_data(&mut header_for(20), "geo/src/a.txt", b"alpha trait CoordNum".as_slice()).unwrap();
            b.append_data(&mut header_for(22), "geo/src/b.txt", b"beta pub fn intersects".as_slice()).unwrap();
            // 150 КиБ СЖИМАЕМОГО исходника (повторяющиеся строки Rust-кода)
            let line = b"pub fn intersects(p: &Point<C>, q: &Point<C>, out: &mut Vec<Coord>) -> bool {\n";
            let big: Vec<u8> = line.repeat(3600);
            b.append_data(&mut header_for(big.len() as u64), "geo/src/geometry/c.rs", big.as_slice()).unwrap();
            b.finish().unwrap();
        }
        let tar_len = tar_bytes.len();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar_bytes).unwrap();
        (gz.finish().unwrap(), tar_len)
    }

    fn header_for(data_len: u64) -> tar::Header {
        let mut h = tar::Header::new_gnu();
        h.set_size(data_len);
        h.set_mode(0o644);
        h.set_cksum();
        h
    }

    /// Сетевой путь (--stream-download): gzip-стрим раскладывается в
    /// .poler с ПОФАЙЛОВОЙ таблицей и сжатием, сырой tar.gz не материализуется.
    #[test]
    fn ingest_reader_gz_stream_per_file_table() {
        let (gz, tar_len) = tar_gz_bytes();
        let p = tmp("net_gz.poler");
        let stats = ingest_reader(
            io::Cursor::new(&gz),
            &p,
            cfg(),
            IngestMode::Auto,
            "geo.tar.gz",
        )
        .unwrap();

        assert!(stats.tar_mode, "tar распознан на лету: {stats:?}");
        assert_eq!(stats.files, 3, "пофайловая таблица из tar-стрима");
        assert!(
            stats.total_stored < tar_len as u64,
            "экономия диска: .poler={} < распакованный tar={} (gz={})",
            stats.total_stored,
            tar_len,
            gz.len()
        );

        let r = PolerReader::open(&p).unwrap();
        let names: Vec<&str> = r.files().iter().map(|f| f.name.as_str()).collect();
        for want in ["geo/src/a.txt", "geo/src/b.txt", "geo/src/geometry/c.rs"] {
            assert!(names.contains(&want), "нет {want}: {names:?}");
        }
        // содержимое записи a.txt — байт-в-байт
        let f = r.find_file("geo/src/a.txt").unwrap();
        let mut out = Vec::new();
        r.read_range(f.raw_off, f.raw_len as usize, &mut out).unwrap();
        assert_eq!(out, b"alpha trait CoordNum".as_slice());

        let rep = r.verify().unwrap();
        assert!(rep.all_ok, "verify: {:?}", rep.files_bad);
    }

    /// Сырой не-тар поток → одиночная запись с именем-подсказкой.
    #[test]
    fn ingest_reader_raw_single_entry() {
        let p = tmp("raw.poler");
        let data: Vec<u8> = (0..100_000u32).map(|i| (i * 7 % 253) as u8).collect();
        let stats = ingest_reader(
            io::Cursor::new(&data),
            &p,
            cfg(),
            IngestMode::Auto,
            "blob.bin",
        )
        .unwrap();
        assert!(!stats.tar_mode);
        assert_eq!(stats.files, 1);
        let r = PolerReader::open(&p).unwrap();
        assert_eq!(r.files()[0].name, "blob.bin");
        let mut out = Vec::new();
        let n = r.stream_out(&mut out).unwrap();
        assert_eq!(n, data.len() as u64);
        assert_eq!(out, data, "lossless");
    }

    /// Постоянный архиватор: каталог → .poler с пофайловой таблицей,
    /// права 0o755 сохранены, verify зелёный.
    #[test]
    fn pack_dir_roundtrip() {
        let d = std::env::temp_dir().join(format!("poler-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::create_dir_all(d.join("viz")).unwrap();
        std::fs::write(d.join("bin/poler-engine"), b"ELF-payload-not-real").unwrap();
        std::fs::write(d.join("viz/scene.svg"), b"<svg>hello</svg>").unwrap();
        std::fs::write(d.join("README.md"), b"# build artifacts").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                d.join("bin/poler-engine"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }

        let p = tmp("build.poler");
        let stats = pack_dir(&d, &p, cfg(), false, false).unwrap();
        assert!(stats.tar_mode);
        assert_eq!(stats.files, 3, "файлы каталога в таблице");

        let r = PolerReader::open(&p).unwrap();
        assert!(r.find_file("bin/poler-engine").is_some());
        assert!(r.find_file("viz/scene.svg").is_some());
        assert!(r.find_file("README.md").is_some());
        let rep = r.verify().unwrap();
        assert!(rep.all_ok, "verify: {:?}", rep.files_bad);

        // extract_all восстанавливает дерево
        let out_dir = tmp("build_unpacked");
        let _ = std::fs::remove_dir_all(&out_dir);
        r.extract_all(&out_dir).unwrap();
        assert_eq!(
            std::fs::read(out_dir.join("bin/poler-engine")).unwrap(),
            b"ELF-payload-not-real"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Грабля Сессии-12: выход .poler ВНУТРИ упаковываемого каталога.
    /// Раньше растущий .part + sidecar попадали в обход, а реальные
    /// файлы терялись молча (архив «сам-себя»). Теперь вывод
    /// и его sidecar-файлы исключаются из обхода.
    #[test]
    fn pack_dir_output_inside_tree_not_packed() {
        let d = std::env::temp_dir().join(format!(
            "poler-selfref-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("first.txt"), b"AAA").unwrap();
        std::fs::write(d.join("second.txt"), b"BBB").unwrap();

        // Вывод — в тот же каталог, который пакуем.
        let out = d.join("same_dir.poler");
        let stats = pack_dir(&d, &out, cfg(), false, false).unwrap();
        assert_eq!(stats.files, 2, "только реальные файлы, без self-reference");

        let r = PolerReader::open(&out).unwrap();
        let names: Vec<&str> = r.files().iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"first.txt"), "{names:?}");
        assert!(names.contains(&"second.txt"), "{names:?}");
        for name in &names {
            assert!(
                !name.contains("same_dir.poler"),
                "свой вывод в архиве: {name}"
            );
        }
        let rep = r.verify().unwrap();
        assert!(rep.all_ok, "verify: {:?}", rep.files_bad);
        let _ = std::fs::remove_dir_all(&d);
    }
}
