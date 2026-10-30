#!/usr/bin/env python3
"""Replace message lines of a script_text.mrg (for test text in knife's mod folder or an emulator's LayeredFS).

Usage: script_text.py SRC OUT LINES [--slot N]

LINES is a UTF-8 file of `index<TAB>text` rows (as `kdu lines` prints them), one per line feed; the text goes
in as is, so a U+3000, a control character or markup stays what it is in the game's lines. Slot N (default 0: the Japanese build's text, the
overseas build's 0-3 by language) is the pair of entries 2N (big-endian line offsets) and 2N + 1 (the lines,
each ending in CR LF). The container (mrgd00) is written again with each entry from a 2048-byte sector.
"""
import struct
import sys


def entries(d):
    count, = struct.unpack_from("<H", d, 6)
    base = 8 + 8 * count
    out = []
    for i in range(count):
        sector, off, sectors, low = struct.unpack_from("<4H", d, 8 + 8 * i)
        size = low if sectors == 0 else (sectors - 1) // 32 * 0x10000 + low
        start = base + (sector << 11) + ((off & 0xFFF) | (off >> 12) << 27)
        out.append(d[start:start + size])
    return out


def pack(blobs):
    head = bytearray(b"mrgd00" + struct.pack("<H", len(blobs)))
    body = bytearray()
    for b in blobs:
        sectors = -(-len(b) // 2048)
        head += struct.pack("<4H", len(body) >> 11, 0, sectors, len(b) & 0xFFFF)
        body += b + bytes(-len(b) % 2048)
    return bytes(head + body)


def main():
    args = sys.argv[1:]
    slot = 0
    if "--slot" in args:
        i = args.index("--slot")
        slot = int(args[i + 1])
        del args[i:i + 2]
    if len(args) != 3:
        raise SystemExit(__doc__)
    src, dst, rows = args
    blobs = entries(open(src, "rb").read())
    offsets, text = blobs[2 * slot], blobs[2 * slot + 1]
    starts = [struct.unpack_from(">I", offsets, 4 * i)[0] for i in range(len(offsets) // 4)]
    # a line runs to the next one's start; 0xFFFFFFFF marks a missing line
    present = sorted({s for s in starts if s != 0xFFFFFFFF})
    end = {s: (present[i + 1] if i + 1 < len(present) else len(text)) for i, s in enumerate(present)}
    lines = [None if s == 0xFFFFFFFF else text[s:end[s]] for s in starts]
    # rows end at line feeds only, so a line can hold other control characters
    for row in open(rows, encoding="utf-8", newline="").read().split("\n"):
        if row.strip():
            index, line = row.split("\t", 1)
            lines[int(index)] = line.encode() + b"\r\n"
    out, table = bytearray(), bytearray()
    for line in lines:
        table += struct.pack(">I", 0xFFFFFFFF if line is None else len(out))
        if line is not None:
            out += line
    blobs[2 * slot], blobs[2 * slot + 1] = bytes(table), bytes(out)
    open(dst, "wb").write(pack(blobs))


if __name__ == "__main__":
    main()
