#!/usr/bin/env python3
"""Build a Switch savedata file whose save list holds test points.

    merge_points.py BASE OUT DIR...

BASE gives the system data and the auto / quick saves; each DIR's savedata.bin (written by
`knife --scene N --save-pc PC`) gives its slot 99 record, placed in slots 0, 1, ... with dates one minute apart,
newest first, so the load list shows them in DIR order. The other list slots are emptied.
"""

import os
import sys

REC_BASE, REC_STRIDE, DATE_AT = 6144, 0x7200, 0x7138


def rec(buf, n):
    o = REC_BASE + REC_STRIDE * n
    return o, o + REC_STRIDE


def main():
    base, out, dirs = sys.argv[1], sys.argv[2], sys.argv[3:]
    d = bytearray(open(base, "rb").read())
    for n in range(100):
        a, b = rec(d, n)
        d[a:b] = bytes(b - a)
    for i, src in enumerate(dirs):
        s = open(os.path.join(src, "savedata.bin"), "rb").read()
        a, b = rec(d, 99)
        r = bytearray(s[a:b])
        if int.from_bytes(r[2:4], "little") == 0:
            raise SystemExit(f"{src}: slot 99 is empty")
        # 2026/10/21 12:59 - i
        r[DATE_AT : DATE_AT + 7] = (2026).to_bytes(2, "little") + bytes([10, 21, 12, 59 - i, 0])
        a, b = rec(d, i)
        d[a:b] = r
    with open(out, "wb") as f:
        f.write(d)
    print(out, len(dirs), "points")


if __name__ == "__main__":
    main()
