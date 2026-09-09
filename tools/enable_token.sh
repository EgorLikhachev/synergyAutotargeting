#!/bin/bash
# enable_token: включить/сменить/выключить токен канала управления (ADR-020)
# на борту. Для ретеста заказчиком (Этап 3): пульт запускать через
# dist/operator-ui/run.bat (он ставит SYNERGY_TOKEN=bench-synergy).
#
# Запуск: ssh radxa@192.168.0.224 'bash ~/synergy/tools/enable_token.sh bench-synergy'
#   "" (пустой аргумент) = выключить токен.
#
# ВНИМАНИЕ: рестартит synergy.service — НЕ запускать во время soak/лёта.
set -euo pipefail
TOKEN="${1:?использование: enable_token.sh <token>|\"\"}"
CFG="$HOME/synergy/config.toml"

cp "$CFG" "$CFG.bak.$(date +%Y%m%d_%H%M%S)"

if grep -q '^token' "$CFG"; then
    sed -i "s|^token.*|token = \"$TOKEN\"|" "$CFG"
elif grep -q '^\[control\]' "$CFG"; then
    awk -v tok="$TOKEN" '
        /^\[control\]/ && !ins { print; print "token = \"" tok "\""; ins=1; next }
        { print }
    ' "$CFG" > "$CFG.new" && mv "$CFG.new" "$CFG"
else
    echo "ОШИБКА: секция [control] не найдена в $CFG" >&2
    exit 1
fi

sudo systemctl restart synergy
sleep 2
systemctl is-active --quiet synergy || { echo "ОШИБКА: synergy не поднялся"; exit 1; }
echo "OK: token=\"${TOKEN}\", synergy активен; проверка: пульт c SYNERGY_TOKEN=$TOKEN"
grep -n '^token' "$CFG"
