#!/bin/bash
# netwatch: сетевое здоровье борта (Этап 1 плана стендовой недели).
# Каждые 30 с: пинг шлюза + TCP-проба собственного sshd. В journal —
# только смены состояния + ежечасный heartbeat: при деградации получаем
# точную timeline.
#
# Автовосстановление МЯГКОЙ деградации (пинг жив, TCP мёртв >5 мин):
# рестарт systemd-networkd (безопасно, сфера не трогается). Жёсткое
# зависание ядра (как 2026-09-08 18:00) этим не лечится — ловится только
# внешним наблюдателем (PC-side cron).
set -u
GW=$(ip route | awk '/default/ {print $3; exit}')
[ -z "$GW" ] && GW=192.168.0.1
state="init"
beats=0
degraded=0
echo "netwatch: шлюз $GW, период 30 с"
while true; do
    ping_ok=0
    ping -c 1 -W 2 "$GW" >/dev/null 2>&1 && ping_ok=1
    tcp_ok=0
    timeout 2 bash -c 'exec 3<>/dev/tcp/127.0.0.1/22' 2>/dev/null && tcp_ok=1
    new="ping=$ping_ok tcp=$tcp_ok"
    if [ "$new" != "$state" ]; then
        echo "netwatch: СМЕНА $state -> $new (uptime $(cut -d. -f1 /proc/uptime)c, $(date -u +%FT%TZ))"
        state="$new"
    fi
    # мягкая деградация: линк/ARP живы, TCP нет
    if [ "$ping_ok" = 1 ] && [ "$tcp_ok" = 0 ]; then
        degraded=$((degraded + 1))
        if [ $degraded -eq 10 ]; then
            echo "netwatch: ВОССТАНОВЛЕНИЕ — TCP мёртв 5 мин при живом пинге, рестарт systemd-networkd"
            systemctl restart systemd-networkd 2>/dev/null
        elif [ $degraded -eq 30 ]; then
            echo "netwatch: КРИТИЧНО — TCP мёртв 15 мин после рестарта networkd; требуется ручное вмешательство (журнал: conntrack/nft)"
        fi
    else
        degraded=0
    fi
    beats=$((beats + 1))
    if [ $((beats % 120)) -eq 0 ]; then
        echo "netwatch: жив, $state, uptime $(cut -d. -f1 /proc/uptime)c"
    fi
    sleep 30
done
