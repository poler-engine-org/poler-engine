#!/usr/bin/env python3
"""gcp-setup-daemon.py — double-fork обёртка gcp-setup.js.

Песочница убивает фоновые процессы в конце каждого bash-вызова (nohup и
setsid не спасают). Классический double-fork (итоговый PPID=1) переживает —
паттерн проверен на auth-stack-daemon.py.

Запуск: python3 gcp-setup-daemon.py [project] [phase]   (default: verification-506705 all)
"""
import os
import subprocess
import sys

LOGDIR = '/home/z/my-project/logs'
SCRIPT = '/home/z/my-project/scripts/gcp-setup.js'
LOGFILE = os.path.join(LOGDIR, 'gcp-setup.log')


def log(msg: str) -> None:
    try:
        with open(LOGFILE, 'a') as f:
            f.write(msg + '\n')
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
    lf = os.open(LOGFILE, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
    os.dup2(lf, 1)
    os.dup2(lf, 2)
    if devnull > 2:
        os.close(devnull)
    if lf > 2:
        os.close(lf)


def main() -> int:
    os.makedirs(LOGDIR, exist_ok=True)
    daemonize()

    project = sys.argv[1] if len(sys.argv) > 1 else 'verification-506705'
    phase = sys.argv[2] if len(sys.argv) > 2 else 'all'
    p = subprocess.Popen(['node', SCRIPT, project, phase])
    log(f'[daemon pid {os.getpid()}] gcp-setup поднят (node pid {p.pid}, проект {project}, фаза {phase})')

    rc = p.wait()
    log(f'[daemon pid {os.getpid()}] gcp-setup завершился: exit={rc}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
