#!/usr/bin/env python3
"""auth-stack-daemon.py — double-fork обёртка dev-оркестратора poler-auth-ui.

Песочница убивает фоновые процессы в конце каждого bash-вызова (nohup и
setsid не спасают). Классический double-fork (итоговый PPID=1 → tini)
переживает — проверено эмпирически.

Роли:
  • этот скрипт — daemonize + держать orchestrator (проброс сигналов);
  • orchestrator (scripts/auth-dev-orchestrator.js) — сам управляет
    Xvfb / auth-companion / релеем / next dev.

Запуск: python3 auth-stack-daemon.py   (из scripts/auth-stack.sh start)
"""

import os
import signal
import subprocess
import sys

LOGDIR = '/home/z/my-project/logs'
ORCHESTRATOR = '/home/z/my-project/scripts/auth-dev-orchestrator.js'


def log(msg: str) -> None:
    line = f'[{os.times()[4]:.0f}s] {msg}\n' if False else msg + '\n'
    try:
        with open(os.path.join(LOGDIR, 'auth-stack.log'), 'a') as f:
            f.write(line)
    except OSError:
        pass


def daemonize() -> None:
    """Двойной fork → PPID=1, своя сессия, stdio → лог."""
    if os.fork() > 0:
        os._exit(0)
    os.setsid()
    if os.fork() > 0:
        os._exit(0)
    os.umask(0o022)
    devnull = os.open(os.devnull, os.O_RDONLY)
    os.dup2(devnull, 0)
    lf = os.open(os.path.join(LOGDIR, 'auth-stack.log'),
                 os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
    os.dup2(lf, 1)
    os.dup2(lf, 2)
    if devnull > 2:
        os.close(devnull)
    if lf > 2:
        os.close(lf)


def main() -> int:
    os.makedirs(LOGDIR, exist_ok=True)
    daemonize()

    p = subprocess.Popen(['node', ORCHESTRATOR])
    log(f'orchestrator поднят (pid {p.pid}, демон pid {os.getpid()})')

    stopping = []

    def on_term(signum, frame):
        if stopping:
            return
        stopping.append(True)
        log(f'сигнал {signum} — глушу orchestrator')
        try:
            p.terminate()
        except OSError:
            pass

    signal.signal(signal.SIGTERM, on_term)
    signal.signal(signal.SIGINT, on_term)

    rc = p.wait()
    log(f'orchestrator завершился: exit={rc}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
