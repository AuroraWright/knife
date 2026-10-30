#!/usr/bin/env python3
"""Differential fuzzing of knife's AA[] compressed deltas (knife-core/src/itype/fnt.rs) against the SDK's own
handler (sdk.py under unicorn).

Usage: aa_fuzz.py 252|253 COUNT SEED [-v]

Random glyphs, vectors, sizes and loop counts; for 252 mostly well-formed code streams built from fnt.rs's
DELTA_CODES, for 253 random words and headers. Each case runs in the executable, then all of them in knife
(fnt.rs's exe_cases test), and the stacks, loop counts, points and touch flags are compared. Cases where the
executable strays outside the glyph's arrays or below the stack ('underflow': memory the SDK would read from its
neighbours) must be errors in knife; the counts are printed with up to five mismatches (-v: also cases knife
refuses that the executable runs).
"""
import os
import random
import re
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import sdk  # noqa: E402

FNT = os.path.join(sdk.ROOT, "knife-core", "src", "itype", "fnt.rs")


def codes():
    """DELTA_CODES as (length, bits, kind, args)."""
    src = open(FNT).read()
    table = src[src.index("const DELTA_CODES"):]
    table = table[:table.index("];")]
    out = [(int(n), b, k, [int(v) for v in a.split(",") if v.strip()])
           for n, b, k, a in re.findall(r"\((\d+), 0b([01]+), Delta::(\w+)(?:\(([\d, ]*)\))?\)", table)]
    assert len(out) == 191, len(out)
    return out


def stream(rnd, table, npts, ncont, ppem):
    """A 252 code stream that mostly stays inside the glyph, then its end code and random padding, as stack words
    (the first word on top)."""
    bits, p, size = "", 0, 5
    for _ in range(rnd.randrange(1, 60)):
        n, b, k, a = rnd.choice(table)
        if k == "End" or k in ("Same", "Point", "Run") and p + (a[0] if k == "Run" else 1) > npts - 1:
            continue
        if k == "Contours" and p + a[0] > ncont or k == "Skip" and p + a[0] > npts - 2:
            continue
        if k == "Size" and rnd.random() < 0.5 and size + a[0] > ppem + 3:
            continue
        bits += b
        if k in ("Same", "Point", "Run"):
            p += 1
        elif k == "Skip":
            p += a[0]
        elif k == "Size":
            size, p = size + a[0], 0
        elif k == "Amount":
            size, p = 5, 0
    bits += next(b for _, b, k, _ in table if k == "End")
    bits += "".join(rnd.choice("01") for _ in range(-len(bits) % 16 + (16 if rnd.random() < 0.3 else 0)))
    words = [int(bits[i:i + 16], 2) | (rnd.getrandbits(16) << 16 if rnd.random() < 0.1 else 0)
             for i in range(0, len(bits), 16)]
    return [w - (1 << 32) if w >= 1 << 31 else w for w in words][::-1]


def case(rnd, sel, table):
    npts = rnd.choice([3000, 40, 7])
    ncont = max(1, npts // rnd.choice([3, 5, 1]))
    cuts = sorted(rnd.sample(range(1, npts), min(ncont - 1, npts - 1)))
    ep = [c - 1 for c in cuts] + [npts - 1]
    spread = rnd.choice([3, 20, 1000])
    ox = [rnd.randrange(-spread, spread) * 64 for _ in range(npts)]
    oy = [rnd.randrange(-spread, spread) * 64 for _ in range(npts)]
    xs = [v + rnd.randrange(-50, 50) for v in ox]
    ys = [v + rnd.randrange(-50, 50) for v in oy]
    on = [rnd.randrange(2) for _ in range(npts)]
    ppem = rnd.choice([5, 6, 7, 8, 9, 12, 16, 20, 25, 40, rnd.randrange(2, 300)])
    pv = rnd.choice([(0x4000, 0), (0, 0x4000), (0x2D41, 0x2D41), (-0x4000, 0), (0x3B21, -0x187E)])
    fv = rnd.choice([(0x4000, 0), (0, 0x4000), (0x2D41, 0x2D41)])
    loop = rnd.choice([0, 0, 0, 2, -1])
    words = [rnd.getrandbits(16) | (rnd.getrandbits(16) << 16 if rnd.random() < 0.2 else 0)
             for _ in range(rnd.choice([4, 20, 80]))]
    words = [w - (1 << 32) if w >= 1 << 31 else w for w in words]
    if sel == 252:
        # the header: shift and amount in one word, or 0x80 in the high byte and the amount popped
        w = rnd.getrandbits(16)
        head = [w] if (w & 0x80FF) != 0x8000 else [rnd.randrange(-500, 500), w]
        if rnd.random() < 0.8:
            words = stream(rnd, table, npts, len(ep), ppem)
        stack = [rnd.randrange(-9, 9)] + words + head + [252]
    else:
        # the header: resume position, first size (flags above), step, amount
        first = rnd.choice([5, 6, ppem, ppem - 1, rnd.randrange(0, 256)]) & 0xFF \
            | (rnd.randrange(16) << 9 if rnd.random() < 0.3 else 0)
        resume = (rnd.randrange(0, 64) << 10 | rnd.randrange(0, 4)) if rnd.random() < 0.3 else 0
        stack = [rnd.randrange(-9, 9)] + words + [resume, first, rnd.choice([64, -64, 32, 1]),
                                                  rnd.choice([64, 128, -64, 0]), 253]
    return ppem, pv, fv, loop, stack, xs, ys, ox, oy, on, ep


def main():
    args = [a for a in sys.argv[1:] if a != "-v"]
    if len(args) != 3 or args[0] not in ("252", "253"):
        raise SystemExit(__doc__)
    sel, count, seed = int(args[0]), int(args[1]), int(args[2])
    verbose = "-v" in sys.argv
    rnd, table, exe = random.Random(seed), codes(), sdk.Interp()
    cases, lines, want = [], [], []
    for _ in range(count):
        c = case(rnd, sel, table)
        ppem, pv, fv, loop, stack, xs, ys, ox, oy, on, ep = c
        want.append(exe.aa(stack, ppem, pv, fv, loop, xs, ys, ox=ox, oy=oy, on=on, ep=ep))
        cases.append(c)
        j = lambda v: " ".join(map(str, v))
        lines.append("|".join([j([ppem, *pv, *fv, loop]), j(stack), j(xs), j(ys), j(ox), j(oy), j(on), j(ep)]))
    with tempfile.TemporaryDirectory() as d:
        path, out = os.path.join(d, "cases.txt"), os.path.join(d, "out.txt")
        open(path, "w").write("\n".join(lines) + "\n")
        env = dict(os.environ, KNIFE_AA_CASES=path, KNIFE_AA_OUT=out, CARGO_INCREMENTAL="0")
        subprocess.run(["cargo", "test", "-q", "--release", "-p", "knife-core", "--lib", "itype::fnt::tests::exe_cases"],
                       cwd=sdk.ROOT, env=env, check=True, capture_output=True)
        got = open(out).read().splitlines()
    stats = dict.fromkeys(("match", "mismatch", "underflow", "both_error", "knife_error", "exe_fault"), 0)
    shown = 0
    for i, (g, w) in enumerate(zip(got, want)):
        if w[0] == "underflow":
            stats["underflow"] += 1
            if g != "error":
                stats["mismatch"] += 1
                print("knife ran a case the executable strays in:", i, lines[i][:160])
            continue
        if w[0] == "fault":
            stats["exe_fault"] += 1
            continue
        if g == "error":
            stats["both_error" if w[0] == "error" else "knife_error"] += 1
            if w[0] == "ok" and verbose and shown < 5:
                shown += 1
                print("knife error, executable ok:", i, lines[i][:160])
            continue
        _, st, loop, xs, ys, fl = g.split("|")
        k = [[int(v) for v in st.split()], int(loop), *([int(v) for v in a.split()] for a in (xs, ys, fl))]
        if w[0] == "ok" and k == list(w[1:]):
            stats["match"] += 1
            continue
        stats["mismatch"] += 1
        if shown < 5:
            shown += 1
            if w[0] != "ok":
                print("MISMATCH", i, "executable", w[0], "knife ok")
            else:
                diff = [p for p in range(len(k[2])) if (k[2][p], k[3][p]) != (w[3][p], w[4][p])]
                print("MISMATCH", i, "stack", k[0][-6:], w[1][-6:], "loop", k[1], w[2], "points", diff[:10])
            print("   ", lines[i][:160])
    print(stats)
    sys.exit(1 if stats["mismatch"] else 0)


if __name__ == "__main__":
    main()
