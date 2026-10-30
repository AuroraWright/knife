"""The SDK's iType font engine (re/sdk.elf, tools/nso2elf.py of switch_exefs/uncompressed_sdk) run under unicorn,
to check knife's ports against the executable itself. Needs `pip install unicorn`.

The image is loaded at 0 as IDA shows it (relocations applied); imports stay unresolved, so only code that calls
nothing outside the module runs.
"""
import os
import struct

from unicorn import UC_ARCH_ARM64, UC_HOOK_CODE, UC_HOOK_MEM_READ, UC_HOOK_MEM_WRITE, UC_MODE_ARM, Uc, UcError
from unicorn.arm64_const import UC_ARM64_REG_LR, UC_ARM64_REG_PC, UC_ARM64_REG_SP, UC_ARM64_REG_X0

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SDK = os.path.join(ROOT, "re", "sdk.elf")

AA = 0x181BA0  # fnt_AA's handler: selector 11 autohints, 252 / 253 the compressed deltas
ERR = 0x183F40  # the interpreter's error exit (longjmp)
RET, HEAP, STACK = 0x7000000, 0x10000000, 0x20000000


def load(u, path=SDK):
    """Map the ELF's PT_LOAD segments (memsz, so .bss is zero) and apply its relocations: relative ones, and
    absolute ones to the symbols the module defines (imports stay 0)."""
    d = open(path, "rb").read()
    phoff, = struct.unpack_from("<Q", d, 32)
    count, = struct.unpack_from("<H", d, 56)
    dynamic = None
    for i in range(count):
        kind, _, off, va, _, filesz, memsz, _ = struct.unpack_from("<IIQQQQQQ", d, phoff + 56 * i)
        if kind == 1:
            start, end = va & ~0xFFF, (va + memsz + 0xFFF) & ~0xFFF
            u.mem_map(start, end - start)
            u.mem_write(va, d[off:off + filesz])
        elif kind == 2:
            dynamic = (off, filesz)
    tags = {}
    off, size = dynamic
    for i in range(0, size, 16):
        tag, val = struct.unpack_from("<qQ", d, off + i)
        if tag == 0:
            break
        tags.setdefault(tag, val)
    symtab = tags[6]  # DT_SYMTAB

    def symbol(i):
        _, _, _, shndx, value, _ = struct.unpack("<IBBHQQ", u.mem_read(symtab + 24 * i, 24))
        return value if shndx else None

    # DT_RELA / DT_RELASZ and DT_JMPREL / DT_PLTRELSZ
    for table, size in ((tags[7], tags[8]), (tags.get(23), tags.get(2))):
        if not table:
            continue
        rela = bytes(u.mem_read(table, size))
        for i in range(0, len(rela), 24):
            slot, info, addend = struct.unpack_from("<QQq", rela, i)
            kind, sym = info & 0xFFFFFFFF, info >> 32
            if kind == 1027:  # R_AARCH64_RELATIVE
                u.mem_write(slot, struct.pack("<q", addend))
            elif kind in (257, 1025, 1026) and symbol(sym) is not None:  # ABS64, GLOB_DAT, JUMP_SLOT
                u.mem_write(slot, struct.pack("<q", symbol(sym) + addend))


class Interp:
    """fnt_AA on a synthetic interpreter state: the glyph is zone 1 for every zone pointer, its arrays apart in a
    heap whose reads and writes outside them (an out-of-range point, contour or stack slot: memory the SDK would
    read from its neighbours) stop the run as 'underflow'."""

    def __init__(self):
        u = Uc(UC_ARCH_ARM64, UC_MODE_ARM)
        load(u)
        u.mem_map(HEAP, 0x100000)
        u.mem_map(STACK, 0x100000)
        u.mem_map(RET, 0x1000)
        self.u = u
        self.err = self.stray = False
        self.ok_r = self.ok_w = []

        def stop(uc, addr, size, _):
            self.err = addr == ERR
            uc.emu_stop()

        u.hook_add(UC_HOOK_CODE, stop, begin=ERR, end=ERR)
        u.hook_add(UC_HOOK_CODE, stop, begin=RET, end=RET)

        def checked(allowed):
            def hook(uc, access, addr, size, value, _):
                if not any(a <= addr and addr + size <= b for a, b in allowed()):
                    self.stray = True
                    uc.emu_stop()
            return hook

        # the arrays (HEAP + 0x3000..), then below the stack's base (HEAP + 0x30000..0x40000)
        u.hook_add(UC_HOOK_MEM_READ, checked(lambda: self.ok_r), begin=HEAP + 0x3000, end=HEAP + 0x30000 - 1)
        u.hook_add(UC_HOOK_MEM_WRITE, checked(lambda: self.ok_w), begin=HEAP + 0x3000, end=HEAP + 0x30000 - 1)
        u.hook_add(UC_HOOK_MEM_READ, checked(lambda: []), begin=HEAP + 0x30000, end=HEAP + 0x40000 - 1)

    def aa(self, stack, ppem, pv, fv, loop, xs, ys, ox=None, oy=None, on=None, ep=None):
        """`stack` bottom first, the selector on top; the glyph's current x / y, original ox / oy (both original
        sets), on-curve flags and contour ends. Returns ('ok', stack, loop, x, y, flags), ('error',) for the
        interpreter's own error, ('underflow',) for a stray access or ('fault', message, pc)."""
        u = self.u
        n = len(xs)
        ox = xs if ox is None else ox
        oy = ys if oy is None else oy
        on = [1] * n if on is None else on
        ep = [n - 1] if ep is None else ep
        local, globs, elements = HEAP, HEAP + 0x1000, HEAP + 0x2000
        u.mem_write(HEAP, b"\0" * 0x100000)
        arrays, at = {}, HEAP + 0x3000

        def put(name, data):
            nonlocal at
            arrays[name] = at
            u.mem_write(at, data)
            at += ((len(data) + 15) & ~15) + 0x1000

        for name, v in (("x", xs), ("y", ys), ("ox", ox), ("oy", oy), ("oox", ox), ("ooy", oy)):
            put(name, struct.pack(f"<{n}i", *v))
        put("on", bytes(on))
        put("f", bytes(n))
        put("sp", struct.pack(f"<{len(ep)}H", *([0] + [e + 1 for e in ep[:-1]])))
        put("ep", struct.pack(f"<{len(ep)}H", *ep))
        span = lambda k, w, c=n: (arrays[k], arrays[k] + w * c)
        self.ok_w = [span("x", 4), span("y", 4), span("f", 1)]
        self.ok_r = [span(k, 4) for k in ("x", "y", "ox", "oy", "oox", "ooy")] + [span("on", 1), span("f", 1),
                                                                               span("sp", 2, len(ep)), span("ep", 2, len(ep))]
        # fnt_ElementType: x y ox oy oox ooy on sp ep f, then the contour count
        glyph = elements + 88
        u.mem_write(glyph, b"".join(struct.pack("<Q", arrays[k]) for k in ("x", "y", "ox", "oy", "oox", "ooy", "on", "sp",
                                                                          "ep", "f")) + struct.pack("<h", len(ep)))
        base = HEAP + 0x40000
        u.mem_write(base, struct.pack(f"<{len(stack)}i", *stack))
        # fnt_LocalGraphicStateType: zone pointers, vectors, stack pointer, elements, globals, loop
        u.mem_write(local, struct.pack("<QQQ", glyph, glyph, glyph))
        u.mem_write(local + 24, struct.pack("<hhhh", pv[0], pv[1], fv[0], fv[1]))
        u.mem_write(local + 40, struct.pack("<Q", base + 4 * len(stack)))
        u.mem_write(local + 56, struct.pack("<Q", elements))
        u.mem_write(local + 64, struct.pack("<Q", globs))
        u.mem_write(local + 84, struct.pack("<i", loop))
        # fnt_GlobalGraphicStateType: stack base, ppem, the scale's stretch, no rotation, the defaults
        u.mem_write(globs + 8, struct.pack("<Q", base))
        u.mem_write(globs + 34, struct.pack("<H", ppem))
        u.mem_write(globs + 248, struct.pack("<ii", 0x10000, 0x10000))
        u.mem_write(globs + 276, b"\0")
        u.mem_write(globs + 348, struct.pack("<I", 0x21404000))
        u.reg_write(UC_ARM64_REG_SP, STACK + 0x80000)
        u.reg_write(UC_ARM64_REG_X0, local)
        u.reg_write(UC_ARM64_REG_LR, RET)
        self.err = self.stray = False
        try:
            u.emu_start(AA, RET, count=5_000_000)
        except UcError as e:
            return ("fault", str(e), hex(u.reg_read(UC_ARM64_REG_PC)))
        if self.stray:
            return ("underflow",)
        if self.err:
            return ("error",)
        top, = struct.unpack("<Q", u.mem_read(local + 40, 8))
        k = (top - base) // 4
        left = list(struct.unpack(f"<{k}i", u.mem_read(base, 4 * k))) if k > 0 else []
        loop, = struct.unpack("<i", u.mem_read(local + 84, 4))
        x = list(struct.unpack(f"<{n}i", u.mem_read(arrays["x"], 4 * n)))
        y = list(struct.unpack(f"<{n}i", u.mem_read(arrays["y"], 4 * n)))
        return ("ok", left, loop, x, y, list(u.mem_read(arrays["f"], n)))
