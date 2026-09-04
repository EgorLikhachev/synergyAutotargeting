# Драйвер Sony PS Eye (ov534) для борта ROCK 5A

Vendor-ядро Radxa (6.1.84-8-rk2410) не содержит модулей gspca. Драйвер
собирается out-of-tree из vanilla-исходников 6.1 (они не хранятся в этом
репозитории — лицензия GPL; скачайте по инструкции).

## Сборка (прямо на борту, заголовки и gcc уже есть)

```sh
mkdir -p ~/pseye && cd ~/pseye
base=https://raw.githubusercontent.com/torvalds/linux/v6.1/drivers/media/usb/gspca
curl -LO $base/gspca.c && curl -LO $base/gspca.h && curl -LO $base/ov534.c
# Makefile — из этого каталога репозитория
make
sudo cp gspca_main.ko gspca_ov534.ko /lib/modules/$(uname -r)/updates/
sudo depmod -a
sudo modprobe gspca_ov534
echo gspca_ov534 | sudo tee /etc/modules-load.d/pseye.conf  # автозагрузка
```

## Параметры камеры

- Узел: `/dev/videoN` (появляется после загрузки модуля).
- Формат: raw Bayer **GRBG** (onecc 'GRBG'), 640×480@60 или 320×240@до 187 fps.
- Конфиг synergy: `format = "grbg"`, см. config.example.toml.
