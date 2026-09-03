#!/bin/sh
# USB reset камеры для борта. Лечит сбойный режим «кадр 3×3» и пропажу
# /dev/video0. Вызывается бортом (synergy) через sudo NOPASSWD —
# единственный разрешённый root-вызов (см. deploy.sh / docs).
#
# Стратегия:
# 1. /dev/video0 есть → soft-reset: unbind/bind ТОЛЬКО интерфейса,
#    к которому привязан video0 (сброс контрольного интерфейса UVC
#    ломает устройство — bind затем падает, проверено на железе).
# 2. /dev/video0 нет → полный сброс USB-устройства через authorized
#    (эквивалент выдёргивания/вставки камеры).
set -u

if [ -e /sys/class/video4linux/video0/device ]; then
    iface="$(basename "$(readlink -f /sys/class/video4linux/video0/device)")"
    echo "$iface" > /sys/bus/usb/drivers/uvcvideo/unbind 2>/dev/null
    sleep 1
    echo "$iface" > /sys/bus/usb/drivers/uvcvideo/bind 2>/dev/null
    sleep 2
else
    # ищем физическое USB-устройство камеры 0c45:6366
    dev=""
    for d in /sys/bus/usb/devices/*; do
        [ -f "$d/idVendor" ] || continue
        if [ "$(cat "$d/idVendor")" = "0c45" ] && [ "$(cat "$d/idProduct")" = "6366" ]; then
            dev="${d##*/}"
            break
        fi
    done
    if [ -z "$dev" ]; then
        echo "камера 0c45:6366 не найдена в USB" >&2
        exit 1
    fi
    echo 0 > "/sys/bus/usb/devices/$dev/authorized"
    sleep 1
    echo 1 > "/sys/bus/usb/devices/$dev/authorized"
    sleep 3
fi

nodes="$(ls /dev/video* 2>/dev/null | tr '\n' ' ')"
echo "uvcvideo reset | узлы: $nodes"
if [ ! -e /dev/video0 ]; then
    echo "/dev/video0 не появился" >&2
    exit 1
fi
