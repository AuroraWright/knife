#!/usr/bin/env python3
"""Fully unlocked system data for testing the flowchart / gallery.

Switch: writes `savedata.bin` (the game's single save file, mounted as save:/savedata.bin). Layout (sub_A8E30,
sub_B67A0, sub_4EC60, sub_4EA30): a 0x3E7000-byte buffer; 6144 bytes of system data at 0 (5952 used), then 140
records of 0x7200 bytes. A block is loaded only if its first u16 is non-zero; the system block also needs
u16 at +2. System block:
  +4      config (off_AE03D8, 104 bytes; static defaults at 0x46B080)
  +108    progress words (off_AE04E0, 1000 bytes): word 5 movie flags, 110+ read nodes (RDST, bits 0..575),
          300+ completed nodes (RDED), 400+ edges (flags 564..1263)
  +1108   seen bitmap (off_AE07A8, 531 bytes): images 0..2917, BGM heard at bit 3200 + n (n < 1048)
  +1639   off_AE0350+13 (4020 bytes, merged by max), +5668 off_AE07B0 (280 bytes): left zero
  +5662   CONTINUE record index, +5666 word_F1F43C
With --patch, an existing savedata.bin keeps its records and settings and only gains the unlock bits.

knife: knife uses the same savedata.bin (OUT_DIR/savedata.bin, run with --save-file), patching an existing one. With
--nodes (`kdu nodes` output: {node: [scene, pc, scene name, offset, next offset]}) record 0, when free, gets a
save at the start of node 1 whose playthrough has every route node and edge (F201.. / F600.., 8 bits per word,
both in the record's F subset), so the system menu's flowchart opens fully lit. Flowchart jumps need nothing
stored per node (the exe restarts the node's scene, sub_178C8).

witch: Witch on the Holy Night's savedata.bin (0x3FB800 bytes, knife/src/savedata.rs `WITCH`, re/NOTES_WOH.md "Save
file"), patching an existing one: a 7168-byte system block (u16 +2 = 1; +4 config, 320 bytes; +324 G, 330 words;
+984 seen, 524 bytes), then the records, kept as they are. It gains what a finished playthrough leaves there:
  G010 = 1  past the first launch (0: the title1st splash starts the story; 1_2 sets it)
  G011..G024 = 1  each voice character met (the config's voice help, sub_B7E0)
  G026 bits 1, 8, 9  the TeaTime menus' "new book in the archive" popups shown (TTCT)
  G130..  RDST bits (bit n % 16 of G130 + n / 16, jp_8D180): every RDST(n) of the scripts, 0..206 but 135 / 137.
          They hold jp_8D1C0's ending bits (the second title; every CG, track and archive scene, jp_455C0 /
          jp_45FE0 / sub_2CD0), the CGs of jp_455C0's switch and the archive's read icons
  seen    images 0..3199 (jp_644A0), BGM n <= 0x207 at bit 3200 + n (jp_77740)
Unlike Tsukihime there is no flowchart, so no record is added.

usage: unlocked_save.py switch OUT.bin [--patch IN.bin]
       unlocked_save.py knife OUT_DIR [--nodes NODES.json]
       unlocked_save.py witch OUT_DIR
"""
import json
import os
import struct
import sys
import time

SIZE = 0x3E7000
SYS = 5952
CONFIG_DEFAULTS = bytes.fromhex(
    "ff0000002800640028000a00000023000f00ffff01000000000000009808010001000000ffffff3f0300000000000100"
    "2800000032003200320032003200320032003200320032003200320032003200320032003200320032000000b80b0000"
    "0000000100000000"
)
NODES = 576  # sub_926B0 / sub_922C0: n <= 0x23F
EDGES = 700  # sub_92300: flags 564..1263
SEEN = 2918  # sub_77610: images 0..2700 by table index, 2701..2917 aliases (sub_77360)
BGM = 1048  # sub_64F60: n <= 0x417
MOVIES = 6


def set_bits(buf, off, start, count):
    for i in range(start, start + count):
        buf[off + (i >> 3)] |= 1 << (i & 7)


def unlocked(data):
    struct.pack_into("<HH", data, 0, 1, 1)
    prog = 108
    # u16 words are little endian, so word w bit b is byte 2w + b/8
    struct.pack_into("<H", data, prog + 2 * 5, struct.unpack_from("<H", data, prog + 10)[0] | (1 << MOVIES) - 1)
    set_bits(data, prog + 2 * 110, 0, NODES)
    set_bits(data, prog + 2 * 300, 0, NODES)
    set_bits(data, prog + 2 * 400, 0, EDGES)
    seen = 1108
    set_bits(data, seen, 0, SEEN)
    set_bits(data, seen, 3200, BGM)


def load(path):
    if path and os.path.exists(path):
        data = bytearray(open(path, "rb").read())
        if len(data) != SIZE:
            sys.exit(f"{path}: expected {SIZE} bytes, got {len(data)}")
        return data
    data = bytearray(SIZE)
    data[4:4 + len(CONFIG_DEFAULTS)] = CONFIG_DEFAULTS
    # word_F1F438 (CONTINUE record) and word_F1F43C are .bss, 0 in a fresh profile (savedata000_introvideo)
    return data


def switch(out, patch):
    data = load(patch) if patch else load(None)
    unlocked(data)
    with open(out, "wb") as f:
        f.write(data)
    print(f"wrote {out} ({len(data)} bytes)")


# save record (see knife/src/savedata.rs): page entry at +8, date at +0x7138
REC, REC_STRIDE, PAGE, LAYERS = 6144, 0x7200, 8, 40
F_ROUTE, F_EDGE = 201, 600
# START_SCRIPT's lasting state: F822 = 1 keeps a page's earlier messages on screen (sub_391A0); without it the
# Switch shows every message in its own window
START_VARS = [["F822", 1]]


def empty_stage():
    s = bytearray(0x19B8)
    struct.pack_into("<h", s, 0, -1)
    for l in range(LAYERS):
        for o, v in ((4, -1), (804, 1000), (1204, 1000), (1924, -1), (2004, 1000), (2084, 1000), (2204, -1), (2284, -1), (2364, -1)):
            struct.pack_into("<h", s, o + 2 * l, v)
        s[124 + l] = 0xFF
        struct.pack_into("<ff", s, 5204 + 16 * l, 1.0, 1.0)
        s[5844 + 18 * l + 6:5844 + 18 * l + 9] = b"\xff\xff\xff"
    s[6564 + 8] = 0x78
    return s


def route_record(scene, off, nxt, f_index):
    """A record at a node start (type 0, no message) with every route bit set in the saved F subset."""
    p = bytearray(15016)
    struct.pack_into("<qqH", p, 8, off, nxt, scene)
    struct.pack_into("<h", p, 34, -1)
    for ch in range(20):
        struct.pack_into("<h", p, 38 + 2 * ch, -1)
        p[14468 + ch] = 100
        p[14612 + 20 * ch:14612 + 20 * ch + 20] = bytes.fromhex("0000ffffffffffff00ffffffffffffff00000000")
    st = empty_stage()
    p[80:80 + len(st)] = st
    p[6664:6664 + len(st)] = st
    struct.pack_into("<i", p, 13752, -1)
    for f in [F_ROUTE + w for w in range(NODES // 8)] + [F_EDGE + w for w in range(-(-EDGES // 8))]:
        k = f_index["bytes"].get(f)
        if k is not None:
            p[13449 + k] = 0xFF
    for f, v in START_VARS:
        k = f_index["bits"][int(f[1:])]
        if v:
            p[13442 + k // 8] |= 1 << (k % 8)
    r = bytearray(0x7140)
    struct.pack_into("<HH", r, 0, 1010, 1)
    r[PAGE:PAGE + len(p)] = p
    t = time.localtime()
    struct.pack_into("<HBBBBB", r, 0x7138, t.tm_year, t.tm_mon, t.tm_mday, t.tm_hour, t.tm_min, t.tm_sec)
    return r


def f_index():
    """Position of each F word in the record's bit and low-byte lists (F_BITS / F_BYTES in
    knife/src/save_tables.rs)."""
    import re
    src = open(os.path.join(os.path.dirname(__file__), "..", "knife", "src", "save_tables.rs")).read()

    def table(name):
        i = src.index(f"pub const {name}")
        body = src[src.index("= [", i) + 3:src.index("];", i)]
        return {int(v): k for k, v in enumerate(re.findall(r"\d+", body))}

    return {"bits": table("F_BITS"), "bytes": table("F_BYTES")}


def knife(out_dir, nodes_path=None):
    path = os.path.join(out_dir, "savedata.bin")
    data = load(path)
    unlocked(data)
    nodes = json.load(open(nodes_path)) if nodes_path else {}
    if "1" in nodes and struct.unpack_from("<H", data, REC + 2)[0] == 0:
        scene, _, _, off, nxt = nodes["1"]
        data[REC:REC + 0x7140] = route_record(scene, off, nxt, f_index())
        struct.pack_into("<h", data, 5662, 0)
        print("record 0: node 1 with every route node and edge")
    os.makedirs(out_dir, exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)
    print(f"wrote {path}")


W_SIZE, W_SYS, W_CONFIG, W_G, W_SEEN = 0x3FB800, 7168, 4, 324, 984
# RDST(n) in the scripts (kdu scenes): 0..206 but 135 and 137
W_RDST = [n for n in range(207) if n not in (135, 137)]
W_IMAGES, W_BGM = 3200, 0x208


def witch_config_defaults():
    """WITCH_CONFIG_DEFAULTS in knife/src/savedata.rs (0x2EB0B0)."""
    import re
    src = open(os.path.join(os.path.dirname(__file__), "..", "knife", "src", "savedata.rs")).read()
    i = src.index("pub const WITCH_CONFIG_DEFAULTS")
    body = src[src.index("= [", i) + 3:src.index("];", i)]
    return bytes(int(v, 0) for v in re.findall(r"0x[0-9a-f]+|\d+", body))


def witch(out_dir):
    path = os.path.join(out_dir, "savedata.bin")
    if os.path.exists(path):
        data = bytearray(open(path, "rb").read())
        if len(data) != W_SIZE:
            sys.exit(f"{path}: expected {W_SIZE} bytes, got {len(data)}")
    else:
        data = bytearray(W_SIZE)
    if struct.unpack_from("<H", data, 2)[0] == 0:
        # an unused system block: a fresh one (sub_4EA30; the language word -1 takes the system language)
        data[:W_SYS] = bytes(W_SYS)
        struct.pack_into("<H", data, 2, 1)
        cfg = witch_config_defaults()
        data[W_CONFIG:W_CONFIG + len(cfg)] = cfg

    def g_or(n, v):
        struct.pack_into("<H", data, W_G + 2 * n, struct.unpack_from("<H", data, W_G + 2 * n)[0] | v)

    g_or(10, 1)
    for n in range(11, 25):
        g_or(n, 1)
    g_or(26, 1 << 1 | 1 << 8 | 1 << 9)
    for n in W_RDST:
        g_or(130 + n // 16, 1 << (n % 16))
    set_bits(data, W_SEEN, 0, W_IMAGES)
    set_bits(data, W_SEEN, W_IMAGES, W_BGM)
    os.makedirs(out_dir, exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)
    print(f"wrote {path}")


if __name__ == "__main__":
    a = sys.argv[1:]
    if len(a) >= 2 and a[0] == "switch":
        switch(a[1], a[3] if len(a) >= 4 and a[2] == "--patch" else None)
    elif len(a) in (2, 4) and a[0] == "knife" and (len(a) == 2 or a[2] == "--nodes"):
        knife(a[1], a[3] if len(a) == 4 else None)
    elif len(a) == 2 and a[0] == "witch":
        witch(a[1])
    else:
        sys.exit(__doc__)
