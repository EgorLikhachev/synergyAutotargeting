set -e
echo "== udev-алиасы камер =="
cat > /etc/udev/rules.d/99-synergy-cams.rules << 'EOF'
SUBSYSTEM=="video4linux", ATTRS{idVendor}=="1415", ATTRS{idProduct}=="2000", SYMLINK+="video-pseye"
SUBSYSTEM=="video4linux", ATTRS{idVendor}=="0c45", ATTRS{idProduct}=="6366", SYMLINK+="video-arducam"
EOF
udevadm control --reload
udevadm trigger --subsystem-match=video4linux
sleep 1
ls -la /dev/video-* 2>/dev/null || echo "алиасы не появились"

echo "== что умеют камеры =="
for d in /dev/video-pseye /dev/video-arducam; do
  echo "--- $d:"
  v4l2-ctl -d $d --info 2>/dev/null | grep -E "Driver|Card" | head -2
  v4l2-ctl -d $d --list-formats-ext 2>/dev/null | grep -E "^\s+\[|Size" | head -6
done

echo "== uart7 overlay? =="
ls /boot/dtb/rockchip/overlay/ | grep -iE "uart7|uart-7" || echo "uart7 в списке нет — смотреть полный лист"

echo "== все rk3588 overlays (uart): =="
ls /boot/dtb/rockchip/overlay/ | grep "^rk3588" | grep -i uart | head -8

echo "== armbianEnv overlays line =="
grep -E "^overlays|^overlay_prefix|^param_" /boot/armbianEnv.txt || echo "(строки overlays нет)"
