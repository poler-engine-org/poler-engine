#!/usr/bin/env python3
"""gcp-e2e-daemon.py — double-fork обёртка gcp-e2e-oauth.js (PPID→1)."""
import os
import subprocess

LOGDIR = '/home/z/my-project/logs'
SCRIPT = '/home/z/my-project/scripts/gcp-e2e-oauth.js'
LOGFILE = os.path.join(LOGDIR, 'gcp-e2e.log')


def log(msg: str) -> None:
    try:
        with open(LOGFILE, 'a') as f:
            f.write(msg + '\n')
    except OSError:
        pass


def daemonize() -> None:
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
    p = subprocess.Popen(['node', SCRIPT])
    log(f'[daemon pid {os.getpid()}] e2e-oauth запущен (node pid {p.pid})')
    rc = p.wait()
    log(f'[daemon pid {os.getpid()}] e2e-oauth завершён: exit={rc}')
    return 0


if __name__ == '__main__':
    main()
