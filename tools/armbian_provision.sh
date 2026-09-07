set -e
echo "== система =="
cat /etc/armbian-release 2>/dev/null | head -4 || true
uname -r

echo "== пользователь radxa =="
id radxa 2>/dev/null || useradd -m -s /bin/bash -G sudo,video radxa
echo "radxa:radxa" | chpasswd
grep -q "^radxa" /etc/sudoers.d/* 2>/dev/null || echo "radxa ALL=(ALL) NOPASSWD: ALL" > /etc/sudoers.d/radxa && chmod 440 /etc/sudoers.d/radxa

echo "== ssh-ключ оператора =="
mkdir -p /root/.ssh /home/radxa/.ssh
cat > /root/.ssh/authorized_keys << 'EOF'
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKjYMIO8qV11bJ32ST663WbSHYJwhIkZbnTIeQasu6n1 egor.likhachev.job@yandex.ru
EOF
cp /root/.ssh/authorized_keys /home/radxa/.ssh/authorized_keys
chmod 700 /root/.ssh /home/radxa/.ssh
chmod 600 /root/.ssh/authorized_keys /home/radxa/.ssh/authorized_keys
chown -R radxa:radxa /home/radxa/.ssh

echo "== пакеты =="
export DEBIAN_FRONTEND=noninteractive
apt-get -qq update 2>&1 | tail -1 || true
apt-get -qq install -y sudo v4l-utils python3 usbutils 2>&1 | tail -2 || true

echo "== USB-камеры =="
lsusb | grep -iE "1415:2000|0c45:6366" || echo "камеры не видны!"

echo "== драйвер PS Eye (gspca in-tree?) =="
modprobe gspca_ov534 2>&1 && echo "gspca_ov534: ЕСТЬ in-tree" || echo "gspca_ov534: НЕТ, нужна сборка"
lsmod | grep -E "gspca|ov534" || true

echo "== NPU =="
dmesg | grep -iE "rknpu|RKNPU" | tail -3 || true
ls /dev/dri/ 2>/dev/null || true
ls -la /usr/lib/librknnrt* 2>/dev/null || echo "librknnrt: нет"

echo "== оверлеи Armbian =="
ls /boot/dtb/rockchip/overlay/ 2>/dev/null | grep -i uart | head -5 || ls /boot/dtb*/rockchip*/overlay* 2>/dev/null | head -5 || echo "структуру dtb посмотреть"
head -20 /boot/armbianEnv.txt 2>/dev/null || true
echo "== ГОТОВО =="
