# zig_probe — регенерация golden-векторов PND v8 из РЕАЛЬНОГО Zig-ядра

MVR-v3, фаза побитовой сверки (цикл A-финал). Инструмент воспроизводим:
среда эфемерна, скрипты — в git.

## Что здесь

- `golden_dump.zig` — харнесс: импортирует probe-копию `poler_core.zig` и
  печатает golden-векторы (phi / pndmix / sbox / mds / lhca / fround / fhalf /
  cipher / prng / modinv / attractor / gfmul) в текстовый формат.

## Процедура регенерации (нужны: клон poler-os + Zig 0.14.0)

```bash
export POLER_OS_PATH=/path/to/poler-os        # клон github.com/poler-engine-org/poler-os
export ZIG_BIN=/path/to/zig                   # Zig 0.14.0

SRC=$POLER_OS_PATH/zig-kernel/src64/poler_core.zig
PROBE=$POLER_OS_PATH/zig-kernel/src64/poler_core_probe.zig

# 1. probe-копия: ОРИГИНАЛ НЕ ТРОГАЕМ, diff = ровно 12 строк pub-алиасов
cp "$SRC" "$PROBE"
cat >> "$PROBE" << 'EOF'

pub const probe_mixColumnsPnd = mixColumnsPnd;
pub const probe_invMixColumnsPnd = invMixColumnsPnd;
pub const probe_polerFeistelF = polerFeistelF;
pub const probe_polerFeistelFHalf = polerFeistelFHalf;
pub const probe_ctGf256Mul = ctGf256Mul;
EOF
diff "$SRC" "$PROBE"   # убедиться: только блок алиасов

# 2. собственные тесты ядра (нетронутый оригинал) — должны быть зелёными
$ZIG_BIN test "$SRC"   # 23/23 OK на Zig 0.14.0

# 3. сборка и запуск дампа (в каталоге с poler_core_probe.zig)
cp golden_dump.zig "$POLER_OS_PATH/zig-kernel/src64/"
cd "$POLER_OS_PATH/zig-kernel/src64"
$ZIG_BIN build-exe golden_dump.zig -O ReleaseFast -femit-bin=/tmp/golden_dump
/tmp/golden_dump > golden_vectors.txt
mv golden_vectors.txt /path/to/poler-engine/tools/verifiers/golden/pnd_v8_golden_54626.txt

# 4. сверка с коммитным кешем
git -C /path/to/poler-engine diff --stat tools/verifiers/golden/
```

## Кеш

`../golden/pnd_v8_golden_54626.txt` (1.8 MB, 54 626 векторов) снят с
`poler-os @ fc3ffa8` (zig-kernel/src64/poler_core.zig, 1881 строка) на
Zig 0.14.0. Включает 272 полных шифрования (16 = собственные тестовые
векторы Zig: 4 ключа × 4 ε) с round-trip флагом.

## Почему probe-копия, а не правка ядра

Приватные функции (mixColumnsPnd, polerFeistelF, ...) недоступны из
другого файла Zig. Probe-копия добавляет ТОЛЬКО pub-алиасы (12 строк),
функции под тестом байт-в-байт идентичны оригиналу — что и подтверждает
diff на шаге 1.
