#!/bin/bash
# auth-stack.sh — управление dev-стендом poler-auth-ui (Next.js + релей + companion)
#   start  — поднять стек (оркестратор: Xvfb + companion + relay:3100 + next:3000)
#   stop   — аккуратно остановить всё
#   status — процессы, порты, состояние companion

set -u
LOGDIR=/home/z/my-project/logs
DAEMON=/home/z/my-project/scripts/auth-stack-daemon.py

STACK_PATTERNS=(
  "auth-stack-daemon.py"
  "auth-dev-orchestrator.js"
  "auth-companion.js"
  "auth-preview.js"
)

stack_pids() {
  local pids=""
  for pat in "${STACK_PATTERNS[@]}"; do
    pids="$pids $(pgrep -f "$pat" 2>/dev/null | tr '\n' ' ')"
  done
  # next dev и его дети
  pids="$pids $(pgrep -f "next dev" 2>/dev/null | tr '\n' ' ')"
  pids="$pids $(pgrep -f "next-server" 2>/dev/null | tr '\n' ' ')"
  pids="$pids $(pgrep -x Xvfb 2>/dev/null | tr '\n' ' ')"
  echo $pids
}

kill_stack() {
  local pids
  pids=$(stack_pids)
  [ -z "$pids" ] && return 0
  # TERM → ждём → KILL
  kill -TERM $pids 2>/dev/null
  for _ in 1 2 3 4 5 6; do
    sleep 0.7
    pids=$(stack_pids)
    [ -z "$pids" ] && return 0
  done
  kill -KILL $pids 2>/dev/null
  sleep 0.5
}

case "${1:-}" in
  start)
    echo "— останавливаю прошлые экземпляры (если были)"
    kill_stack
    rm -f "$LOGDIR/auth-stack.pid"
    echo "— поднимаю dev-оркестратор (double-fork демон)"
    python3 "$DAEMON"
    echo "— жду готовности next dev (:3000)…"
    for i in $(seq 1 60); do
      if curl -s -m 2 -o /dev/null http://127.0.0.1:3000/ 2>/dev/null; then
        echo "✓ next dev отвечает (:3000)"
        break
      fi
      sleep 2
    done
    exec "$0" status
    ;;
  stop)
    kill_stack
    # Chromium изолированного профиля мог остаться от companion
    pkill -f "user-data-dir=/home/z/.cache/poler-engine/google-profile" 2>/dev/null
    echo "стек остановлен"
    ;;
  status)
    echo "=== процессы ==="
    pgrep -af "auth-stack-daemon|auth-dev-orchestrator|auth-companion|auth-preview|Xvfb|next dev" \
      | grep -v grep || echo "(никого)"
    echo "=== companion /status ==="
    curl -sS -m 3 http://127.0.0.1:8765/status 2>&1 | head -c 400; echo
    echo "=== порты ==="
    for p in 3000 3100 81; do
      curl -s -m 3 -o /dev/null -w ":$p → %{http_code}\n" "http://127.0.0.1:$p/" 2>/dev/null ||
        echo ":$p → недоступен"
    done
    echo "=== /api/frame через Next (:3000) ==="
    curl -s -m 4 "http://127.0.0.1:3000/api/frame" 2>/dev/null | head -c 80; echo
    echo "=== хвост dev.log ==="
    tail -5 /home/z/my-project/dev.log 2>/dev/null
    ;;
  *)
    echo "usage: $0 {start|stop|status}"
    exit 1
    ;;
esac
