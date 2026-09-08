#!/bin/bash
# Счётчик байт, записанных процессом synergy в /dev/tty-fc (поле pos у fd).
# При armed растёт ~1140 Б/с (30 Гц x 38 Б) — индикатор стриминга RC.
set -u
P=$(pidof synergy)
[ -z "$P" ] && { echo 'synergy не запущен'; exit 1; }
sudoc() { sudo -n "$@" 2>/dev/null || echo radxa | sudo -S "$@" 2>/dev/null; }
for fd in /proc/$P/fd/*; do
  link=$(sudoc readlink "$fd")
  case "$link" in
    /dev/tty-fc|/dev/ttyACM*)
      echo -n "FD=${fd##*/} "
      sudoc cat /proc/$P/fdinfo/$fd 2>/dev/null | head -1
      exit 0
      ;;
  esac
done
echo 'tty-fc fd не найден'
