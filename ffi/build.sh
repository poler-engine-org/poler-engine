#!/usr/bin/env bash
# =============================================================================
# build.sh — пересборка libp3ffi.so (C-ABI мост P³ → POLER ENGINE)
# =============================================================================
# Цикл R (v0.53.0). Требования: Zig 0.14.0 (https://ziglang.org/download/0.14.0),
# исходники P³ Engine (git clone …; см. P3_FFI_SRC ниже).
#
# Использование:
#   ./ffi/build.sh [путь_к_P3_Engine]          # сборка ReleaseFast + копия сюда
#   P3_FFI_SRC=/path/to/P3_Engine ./ffi/build.sh
#
# Что делает:
#   1. zig build p3-ffi -Doptimize=ReleaseFast  (в P3_Engine)
#   2. Копирует zig-out/lib/libp3ffi.so → ffi/libp3ffi-linux-x86_64.so
#      (эта копия коммитится: Rust-тесты и `p3 conformance` работают
#      без локального P3_Engine)
#
# Результат проверяется живым рукопожатием:
#   poler-engine --exec "p3 info"
#   poler-engine --exec "p3 conformance --pairs 256"
# =============================================================================
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
P3_SRC="${1:-${P3_FFI_SRC:-/home/z/my-project/P3_Engine}}"
ZIG="${ZIG:-zig}"

if ! command -v "$ZIG" >/dev/null 2>&1; then
    echo "❌ Zig не найден. Установите 0.14.0:" >&2
    echo "   curl -sL https://ziglang.org/download/0.14.0/zig-linux-x86_64-0.14.0.tar.xz | tar xJ" >&2
    exit 1
fi

ZIG_VER="$("$ZIG" version)"
if [ "$ZIG_VER" != "0.14.0" ]; then
    echo "⚠️  Внимание: Zig $ZIG_VER, канал сборки P³ — 0.14.0 (продолжаем)" >&2
fi

if [ ! -f "$P3_SRC/src/p3_ffi.zig" ]; then
    echo "❌ $P3_SRC/src/p3_ffi.zig не найден. Передайте путь к P3_Engine:" >&2
    echo "   ./ffi/build.sh /путь/к/P3_Engine" >&2
    exit 1
fi

echo "▸ Сборка libp3ffi.so (ReleaseFast) из $P3_SRC"
( cd "$P3_SRC" && "$ZIG" build p3-ffi -Doptimize=ReleaseFast )

echo "▸ Самопроверка FFI (zig build test-ffi)"
( cd "$P3_SRC" && "$ZIG" build test-ffi )

echo "▸ Копирование в $HERE/libp3ffi-linux-x86_64.so"
cp "$P3_SRC/zig-out/lib/libp3ffi.so" "$HERE/libp3ffi-linux-x86_64.so"

echo "✅ Готово. Проверка из poler-engine:"
echo "   poler-engine --exec 'p3 info'"
