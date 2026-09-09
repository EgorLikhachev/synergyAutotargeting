#!/bin/bash
# soak: 8-часовое наблюдение за бортом (Этап 2, фаза E ROADMAP).
# Каждые 60 с пишет строку: время, uptime, RSS/CPU synergy, температура,
# живость кадрового цикла (обновление WatchdogTimestamp), состояние сети.
# Запуск: nohup bash ~/synergy/tools/soak.sh 8 &   (аргумент — часы)
# Лог: ~/synergy/data/soak_$(date +%Y%m%d_%H%M).log
set -u
HOURS="${1:-8}"
HOURS=$(( ${HOURS%.*} ))  # bash-арифметика целочисленная: 7.5 -> 7
OUT="$HOME/synergy/data/soak_$(date +%Y%m%d_%H%M).log"
mkdir -p "$(dirname "$OUT")"
END=$((SECONDS + HOURS * 3600))
echo "# soak start $(date -u +%FT%TZ), ${HOURS}ч, pid synergy позже" | tee -a "$OUT"
last_wd=""
while [ $SECONDS -lt $END ]; do
    P=$(pidof synergy)
    rss="?"
    cpu="?"
    if [ -n "$P" ]; then
        rss=$(awk '/VmRSS/ {print int($2/1024)}' /proc/$P/status 2>/dev/null)
        cpu=$(awk '{print int(14+$14+$15)}' /proc/$P/stat 2>/dev/null) # utime+stime в тиках/100→%
        cpu=$(( (cpu) * 100 / 100 ))
    fi
    tz=$(awk '{printf "%.1f", $1/1000}' /sys/class/thermal/thermal_zone0/temp 2>/dev/null)
    wd=$(systemctl show synergy -p WatchdogTimestamp --value 2>/dev/null)
    wd_new=" stalled"
    [ "$wd" != "$last_wd" ] && wd_new=" live"
    last_wd="$wd"
    gw=0; ping -c 1 -W 2 192.168.0.1 >/dev/null 2>&1 && gw=1
    echo "$(date -u +%FT%TZ) up=$(cut -d. -f1 /proc/uptime) rss=${rss}M tz=${tz}C wd$wd_new gw=$gw" >> "$OUT"
    sleep 60
done
echo "# soak end $(date -u +%FT%TZ)" | tee -a "$OUT"
