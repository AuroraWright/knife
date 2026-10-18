#!/usr/bin/env python3
"""Convert an uncompressed Switch NSO into an ELF64 that IDA's ELF loader understands.

Usage: nso2elf.py <uncompressed.nso> <out.elf>

Segments are placed so that file offset = vaddr + HDR, with the data segment's memsz
covering .bss. A PT_DYNAMIC header points at the MOD0-referenced dynamic section so
imports and relocations are applied.
"""
import struct
import sys

HDR = 0x1000

PT_LOAD, PT_DYNAMIC = 1, 2
PF_X, PF_W, PF_R = 1, 2, 4


def main(src, dst):
    d = open(src, 'rb').read()
    assert d[:4] == b'NSO0', 'not an NSO'
    segs = []
    for i in range(3):
        file_off, mem_off, size = struct.unpack_from('<III', d, 0x10 + i * 0x10)
        segs.append((file_off, mem_off, size))
    bss_size = struct.unpack_from('<I', d, 0x3c)[0]

    text_off, text_va, _ = segs[0]
    mod0 = struct.unpack_from('<I', d, text_off + 4)[0]
    magic, dyn, bss_start, bss_end = struct.unpack_from('<4siii', d, text_off + mod0)
    assert magic == b'MOD0'
    dyn_va = mod0 + dyn
    bss_end_va = mod0 + bss_end

    image_end = max(m + s for _, m, s in segs)
    out = bytearray(HDR + image_end)
    for f, m, s in segs:
        out[HDR + m:HDR + m + s] = d[f:f + s]

    # find dynamic size by walking until DT_NULL
    p = dyn_va
    while True:
        tag, _val = struct.unpack_from('<qQ', out, HDR + p)
        p += 16
        if tag == 0:
            break
    dyn_size = p - dyn_va

    flags = [PF_R | PF_X, PF_R, PF_R | PF_W]
    phdrs = []
    for i, (_, m, s) in enumerate(segs):
        memsz = s
        if i == 2:
            memsz = max(s, bss_end_va - m, s + bss_size)
        phdrs.append(struct.pack('<IIQQQQQQ', PT_LOAD, flags[i], HDR + m, m, m, s, memsz, 0x1000))
    phdrs.append(struct.pack('<IIQQQQQQ', PT_DYNAMIC, PF_R | PF_W, HDR + dyn_va, dyn_va, dyn_va,
                             dyn_size, dyn_size, 8))

    ehdr = bytearray(64)
    ehdr[:16] = b'\x7fELF\x02\x01\x01' + b'\0' * 9
    struct.pack_into('<HHIQQQIHHHHHH', ehdr, 16,
                     3,        # ET_DYN
                     183,      # EM_AARCH64
                     1,        # version
                     text_va,  # entry
                     64,       # phoff
                     0,        # shoff
                     0,        # flags
                     64, 56, len(phdrs), 64, 0, 0)
    out[:64] = ehdr
    out[64:64 + 56 * len(phdrs)] = b''.join(phdrs)
    open(dst, 'wb').write(out)
    print(f'wrote {dst}: dyn@{dyn_va:#x} size {dyn_size:#x}, bss end {bss_end_va:#x}')


if __name__ == '__main__':
    main(sys.argv[1], sys.argv[2])
