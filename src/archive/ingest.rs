//! Потоковый инжест контейнеров (v0.62.0, IngestPipeline).
//!
//! Автоматическая распаковка на лету входных архивов (.zip, .tar.gz, .tar.zst, .gz, .zst)
//! с детекцией формата по сигнатурам (magic bytes). Распакованный поток данных льется
//! напрямую в FastCDC + BLAKE3 + ZSTD StreamWriter без промежуточного сохранения на диск.

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

    let kind = sniff_magic(&header[..n]);

    match kind {
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
        IngestKind::TarGz | IngestKind::GzSingle => {
            let mut writer = StreamWriter::open(out_path, cfg)?;
            let gz = flate2::read::MultiGzDecoder::new(f);
            let mut reader = std::io::BufReader::with_capacity(256 * 1024, gz);
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                writer.push_bytes(&buf[..n])?;
            }
            let hint = src_path.file_name().unwrap_or_default().to_string_lossy().to_string();
            writer.finish(&hint)
        }
        IngestKind::TarZst | IngestKind::ZstSingle => {
            let mut writer = StreamWriter::open(out_path, cfg)?;
            let zst = zstd::stream::read::Decoder::new(f)?;
            let mut reader = std::io::BufReader::with_capacity(256 * 1024, zst);
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                writer.push_bytes(&buf[..n])?;
            }
            let hint = src_path.file_name().unwrap_or_default().to_string_lossy().to_string();
            writer.finish(&hint)
        }
        _ => {
            super::stream_writer::write_stream(&mut f, out_path, cfg, &src_path.to_string_lossy())
        }
    }
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
