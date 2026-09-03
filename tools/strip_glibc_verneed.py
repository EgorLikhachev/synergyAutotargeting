#!/usr/bin/env python3
"""Убирает из ELF требования версий glibc новее заданной (по умолчанию 2.36).

Кросс-сборка в WSL Ubuntu 24.04 линкуется против sysroot glibc 2.39, а борт
(Debian 12 bookworm) имеет glibc 2.36. Новые Rust-тулчейны ссылаются на
pidfd_spawnp/pidfd_getpid (GLIBC_2.39) КАК НА СЛАБЫЕ символы: достаточно
вырезать Vernaux-записи новых версий из .gnu.version_r и сбросить индексы
версий у таких символов — std при отсутствии pidfd_* откатывается на
posix_spawn, бинарник грузится на старом glibc.

Использование: strip_glibc_verneed.py <elf> [max_minor, напр. 36]
"""
import struct
import sys

SHT_DYNSYM = 11
SHT_GNU_VERNEED = 0x6FFFFFFE
SHT_GNU_VERSYM = 0x6FFFFFFF


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    path = sys.argv[1]
    max_minor = int(sys.argv[2]) if len(sys.argv) > 2 else 36
    data = bytearray(open(path, "rb").read())
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        print("не ELF64 LE — пропускаю")
        return 0
    e_shoff, = struct.unpack_from("<Q", data, 0x28)
    e_shentsize, e_shnum, e_shstrndx = struct.unpack_from("<HHH", data, 0x3A)
    shdrs = []
    for i in range(e_shnum):
        off = e_shoff + i * e_shentsize
        name, typ, flags, addr, offset, size, link, info, align, entsize = \
            struct.unpack_from("<IIQQQQIIQQ", data, off)
        shdrs.append(dict(name=name, typ=typ, offset=offset, size=size,
                          link=link, entsize=entsize))

    dynsym = next((s for s in shdrs if s["typ"] == SHT_DYNSYM), None)
    verneed = next((s for s in shdrs if s["typ"] == SHT_GNU_VERNEED), None)
    versym = next((s for s in shdrs if s["typ"] == SHT_GNU_VERSYM), None)
    if not (dynsym and verneed and versym):
        print("нет dynsym/verneed/versym — не динамический бинарник, пропускаю")
        return 0
    dynstr = shdrs[dynsym["link"]]

    def s(off: int) -> str:
        end = data.index(b"\0", dynstr["offset"] + off)
        return data[dynstr["offset"] + off:end].decode()

    # --- 1. Найти Vernaux с версией новее max_minor и вырезать их ---
    vn_off = verneed["offset"]
    vn_end = vn_off + verneed["size"]
    drop_idx = set()  # vna_other, которые убираем
    removed = []
    prev_group_off = None
    group_off = vn_off
    while group_off < vn_end:
        vn_version, vn_cnt, vn_file, vn_aux, vn_next = \
            struct.unpack_from("<HHIII", data, group_off)
        group_prev_aux = None
        aux_off = group_off + vn_aux
        for _ in range(vn_cnt):
            vna_hash, vna_flags, vna_other, vna_name, vna_next = \
                struct.unpack_from("<IHHII", data, aux_off)
            name = s(vna_name)
            parts = name.rsplit(".", 1)
            minor = None
            if len(parts) == 2 and parts[1].isdigit():
                minor = int(parts[1])
            if minor is not None and minor > max_minor:
                removed.append((s(vn_file), name))
                drop_idx.add(vna_other)
                # вырезать aux из цепочки
                if group_prev_aux is None and vna_next == 0:
                    # единственный aux → вырезаем всю группу:
                    # vn_next предыдущей группы ссылается через нас
                    if prev_group_off is None:
                        print("первая группа оказалась лишней — не поддерживаю")
                        return 1
                    struct.pack_into("<I", data, prev_group_off + 12, vn_next)
                elif group_prev_aux is None:
                    # первый aux в группе: vn_aux -> следующий
                    struct.pack_into("<I", data, group_off + 8, vn_aux + vna_next)
                    struct.pack_into("<H", data, group_off + 2, vn_cnt - 1)
                else:
                    # средний/последний: предыдущий перешагивает удалённый
                    # (vna_next — расстояние, для последнего — 0)
                    struct.pack_into(
                        "<I", data, group_prev_aux + 12,
                        0 if vna_next == 0 else (aux_off - group_prev_aux) + vna_next,
                    )
                    struct.pack_into("<H", data, group_off + 2, vn_cnt - 1)
            else:
                group_prev_aux = aux_off
            if vna_next == 0:
                break
            aux_off += vna_next
        if vn_next == 0:
            break
        prev_group_off = group_off
        group_off += vn_next

    if not drop_idx:
        print(f"ничего не нужно: версий новее 2.{max_minor} нет")
        return 0

    # --- 2. Сбросить индексы версий у затронутых символов ---
    fixed_syms = 0
    nsyms = versym["size"] // 2
    for i in range(nsyms):
        vo = versym["offset"] + i * 2
        idx, = struct.unpack_from("<H", data, vo)
        if idx & 0x7FFF in drop_idx:
            struct.pack_into("<H", data, vo, 1)  # *global* без версии
            fixed_syms += 1

    open(path, "wb").write(data)
    print(f"вырезано версий: {removed}; символов переведено в global: {fixed_syms}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
