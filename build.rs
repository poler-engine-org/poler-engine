// build.rs — M4: сборка Zig-криптоядра (os/core) для фичи `pnd-ffi`.
//
// Без фичи pnd-ffi скрипт — полный no-op: обычная сборка движка
// не требует Zig-тулчейна (как и до M4).
//
// С фичей pnd-ffi порядок разрешения окружения:
//   1. POLER_CORE_LIB=... — готовая директория с libpoler_core.a
//      (например, os/core/zig-out/lib после ручного `zig build`);
//   2. POLER_ZIG=/путь/к/zig — явный путь к бинарнику Zig 0.14.0;
//   3. `zig` в PATH.
use std::env;
use std::path::PathBuf;
use std::process::Command;

fn find_zig() -> Option<PathBuf> {
    if let Some(p) = env::var_os("POLER_ZIG") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(paths) = env::var_os("PATH") {
        for dir in env::split_paths(&paths) {
            let cand = dir.join("zig");
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

fn main() {
    println!("cargo:rerun-if-changed=os/core/poler_core.zig");
    println!("cargo:rerun-if-changed=os/core/abi.zig");
    println!("cargo:rerun-if-changed=os/core/build.zig");
    // Смена env-переменных должна перезапускать скрипт: иначе линкер
    // держит устаревший -L (мина, найденная при смене каталога репо
    // с включённой фичей pnd-ffi — POLER_CORE_LIB указывал в старый клон).
    println!("cargo:rerun-if-env-changed=POLER_CORE_LIB");
    println!("cargo:rerun-if-env-changed=POLER_ZIG");

    if env::var_os("CARGO_FEATURE_PND_FFI").is_none() {
        return; // фича выключена — нулевые требования к окружению
    }

    // 1. Готовая библиотека от пользователя
    if let Some(libdir) = env::var_os("POLER_CORE_LIB") {
        println!("cargo:rustc-link-search=native={}", libdir.to_string_lossy());
        println!("cargo:rustc-link-lib=static=poler_core");
        return;
    }

    // 2/3. Zig из POLER_ZIG или PATH
    let zig = find_zig().unwrap_or_else(|| panic!(
        "фича pnd-ffi требует Zig 0.14.0: установите zig в PATH, задайте\n\
         POLER_ZIG=/путь/к/zig или укажите готовую библиотеку\n\
         POLER_CORE_LIB=os/core/zig-out/lib (после ручного `zig build` в os/core)"
    ));
    let status = Command::new(&zig)
        .args(["build", "-Doptimize=ReleaseSafe"])
        .current_dir("os/core")
        .status()
        .expect("не удалось запустить `zig build` в os/core");
    assert!(status.success(), "zig build (os/core) завершился с ошибкой");

    println!("cargo:rustc-link-search=native=os/core/zig-out/lib");
    println!("cargo:rustc-link-lib=static=poler_core");
}
