const std = @import("std");

// ============================================================================
// POLER Core — сборка как суверенная статическая библиотека (C-ABI слой)
// ============================================================================
//
// Исторически poler_core.zig жил внутри Ring-0 ядра poler-os
// (zig-kernel/src64/). После консолидации монорепозитория (M3) криптографи-
// ческое ядро выделено в os/core/ и собирается как обычная userspace
// статическая библиотека для Linux/macOS/Windows — без голого железа,
// без QEMU, без загрузочных секторов.
//
//   zig build              # → zig-out/lib/libpoler_core.a
//   zig build test         # 23 встроенных crypto-теста (Feistel, phi, SAC, LHCA)
//   zig build -Doptimize=ReleaseFast
//
// Плановый следующий шаг (M4, docs/UNIFIED_ARCHITECTURE.md): export-обвязка
// C-ABI (poler_pnd_mix, poler_feistel_encrypt, ...) и FFI-мост из Rust
// (poler-engine) через extern "C" — нулевой оверхед, нуль внешних
// зависимостей.
// ============================================================================

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    // Статическая библиотека: libpoler_core.a / poler_core.lib
    const lib = b.addStaticLibrary(.{
        .name = "poler_core",
        .root_source_file = b.path("poler_core.zig"),
        .target = target,
        .optimize = optimize,
    });
    b.installArtifact(lib);

    // Встроенные crypto-тесты ядра (зеленые: 23/23, Zig 0.14.0)
    const unit_tests = b.addTest(.{
        .root_source_file = b.path("poler_core.zig"),
        .target = target,
        .optimize = optimize,
    });
    const run_unit_tests = b.addRunArtifact(unit_tests);
    const test_step = b.step("test", "Run poler_core crypto self-tests");
    test_step.dependOn(&run_unit_tests.step);
}
