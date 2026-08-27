//! Zero-copy memory mapping без внешних зависимостей (unix).
//!
//! Прямые объявления `mmap`/`munmap` через `extern "C"`: крейт остаётся
//! zero-dependency, а файл проецируется в память напрямую — без чтения,
//! распаковки и промежуточных копий («instant cold start»).

use std::fs::File;
use std::io;
use std::os::raw::{c_int, c_void};
use std::os::unix::io::AsRawFd;
use std::path::Path;

extern "C" {
    fn mmap(
        addr: *mut c_void,
        length: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, length: usize) -> c_int;
}

const PROT_READ: c_int = 0x1;
const MAP_PRIVATE: c_int = 0x02;

/// Отображение файла только для чтения. Освобождается при `Drop`.
pub struct Mmap {
    ptr: *mut u8,
    len: usize,
}

// Безопасно: отображение неизменяемо, владение эксклюзивное.
unsafe impl Send for Mmap {}
unsafe impl Sync for Mmap {}

impl Mmap {
    /// Спроецировать файл в память (`PROT_READ | MAP_PRIVATE`).
    pub fn open(path: &Path) -> io::Result<Mmap> {
        let file = File::open(path)?;
        let len = file.metadata()?.len() as usize;
        if len == 0 {
            return Ok(Mmap {
                ptr: std::ptr::NonNull::<u8>::dangling().as_ptr(),
                len: 0,
            });
        }
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                PROT_READ,
                MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr as usize == usize::MAX {
            return Err(io::Error::last_os_error());
        }
        Ok(Mmap {
            ptr: ptr as *mut u8,
            len,
        })
    }

    /// Срез поверх отображения — без копирования.
    pub fn as_slice(&self) -> &[u8] {
        if self.len == 0 {
            &[]
        } else {
            // SAFETY: mmap вернул валидный указатель на len байт; доступ только для чтения.
            unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
        }
    }

    /// Размер отображения в байтах.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Отображение пусто?
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Drop for Mmap {
    fn drop(&mut self) {
        if self.len != 0 {
            unsafe {
                munmap(self.ptr as *mut c_void, self.len);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_error() {
        assert!(Mmap::open(Path::new("/nonexistent/pqw/does-not-exist.poler")).is_err());
    }

    #[test]
    fn empty_file_maps_to_empty_slice() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("pqw_mmap_empty.bin");
        std::fs::write(&path, b"").unwrap();
        let m = Mmap::open(&path).unwrap();
        assert!(m.is_empty());
        assert_eq!(m.as_slice(), b"");
        drop(m);
        let _ = std::fs::remove_file(&path);
    }
}
