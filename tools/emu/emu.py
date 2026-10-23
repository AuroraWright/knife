#!/usr/bin/env python3
"""Drive and capture the Switch original running in Astris, and compare it with knife.

Astris must have focus to take input (the commands activate it). Needs `tools/emu/dsu_server.py` running (Astris: Input = DSU Controller, 127.0.0.1:26760, slot 0) and the game
window open. Captures are the game area of the window, scaled to 1920x1080.
EMU_GAME=en drives the overseas build (TSUKIHIME, knife on switch_romfs_en, save copy switch_emu_save_en) instead
of the Japanese one (the default, EMU_GAME=ja).

    emu.py press BTN [ms] | hold BTN | release BTN | stick L|R X Y | reset
    emu.py shot OUT.png
    emu.py run NAME KEYS [--every MS] [--until MS]
        KEYS: comma-separated MS:KEY like knife's --keys (a b x y l r zl zr plus minus up down left right,
        lup/ldown/lleft/lright = left stick tilt for 100 ms, NAME+ / NAME- hold / release, tap/X/Y[/MS] a touch
        at game coordinates, wait); the Switch
        is captured every --every ms (or as fast as a capture allows, 0.25-0.5 s) into build/emu/NAME/emu_NNNN.png,
        with each capture's real time in emu_times.txt
    emu.py knife NAME KEYS [--every MS] [--until MS] [knife args...]
        the same keys through knife (--keys / --shots) on a copy of switch_emu_save(_en), into knife_NNNN.png
        (target/release/knife built with `--features debug`);
        with --title it boots like a normal start (--boot --skip-intro), so the title has its entry state
    emu.py rec NAME KEYS [--fps N] [--until MS]
        the same as run, but records the window (tools/emu/winrec.swift, up to 60 fps, works while covered) and
        keeps a frame every 1000/N ms from input time 0 as emu_NNNN.jpg
    emu.py compare NAME
        build/emu/NAME/compare.png: Switch (left) and knife (right) per capture time
    emu.py tap X Y [MS] | drag X0 Y0 X1 Y1 [MS]
        touch the game screen at 1920x1080 game coordinates (tools/emu/touch.swift: Astris takes the mouse as the
        touch screen; the first touch after focusing only activates it, so a fresh session needs one extra tap)
    emu.py restart [SAVE]
        stop the game (Emulation > Stop), install SAVE as the live save (both Ryujinx save dirs and
        switch_emu_save) while it is stopped, and start it again (double-click in the game list); ~40 s.
        The screen must be unlocked (clicks and the controller do not reach a locked session).
"""

import io
import os
import shutil
import socket
import subprocess
import sys
import threading
import time

from PIL import Image, ImageCms

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
OUT = os.path.join(ROOT, "build", "emu")
WINLIST = os.path.join(OUT, "winlist")
WINREC = os.path.join(OUT, "winrec")
CLICK = os.path.join(OUT, "click")
SAVE_ROOT = os.path.expanduser("~/Library/Containers/V380-Ori.Astris/Data/Library/Application Support/Ryujinx/"
                              "bis/user/save")
# EMU_GAME=ja (default) or en picks the build: (title id, window title prefix, Library row in window-relative pt,
# the local save copy, knife's data). The Library lists the games by title: TSUKIHIME first, then 月姫.
GAMES = {
    "ja": (0x01001DC01486A000, "月姫", (200, 200), "switch_emu_save", "switch_romfs"),
    "en": (0x010057A01E4D4000, "TSUKIHIME", (200, 110), "switch_emu_save_en", "switch_romfs_en"),
}
GAME = os.environ.get("EMU_GAME", "ja")
if GAME not in GAMES:
    raise SystemExit(f"EMU_GAME: {GAME}: expected one of {', '.join(GAMES)}")
TITLE_ID, GAME_TITLE, CLICK_GAME, SAVE_COPY, ROMFS = GAMES[GAME]


def live_save():
    """The game's Ryujinx save directory: the one whose ExtraData0 starts with its title id (u64 LE)."""
    for d in sorted(os.listdir(SAVE_ROOT)):
        try:
            with open(os.path.join(SAVE_ROOT, d, "ExtraData0"), "rb") as f:
                if int.from_bytes(f.read(8), "little") == TITLE_ID:
                    return os.path.join(SAVE_ROOT, d)
        except OSError:
            pass
    raise SystemExit(f"no save directory for {TITLE_ID:016X} (start the game once)")


def focus():
    """Astris reads the controller only while its window is focused."""
    subprocess.run(["osascript", "-e", 'tell application "Astris" to activate'], check=False)
    for _ in range(50):
        front = subprocess.run(["osascript", "-e", 'tell application "System Events" to get name of first process '
                                'whose frontmost is true'], capture_output=True, text=True).stdout.strip()
        if front == "Astris":
            break
        time.sleep(0.1)
    else:
        raise SystemExit("Astris did not come to the front")
    time.sleep(0.5)


def ctrl(*cmds):
    with socket.create_connection(("127.0.0.1", 26761), timeout=10) as s:
        f = s.makefile("rw")
        for c in cmds:
            f.write(c + "\n")
            f.flush()
            r = f.readline().strip()
            if not r.startswith("ok"):
                raise SystemExit(f"dsu: {c}: {r}")
        return r


def window():
    if not os.path.exists(WINLIST):
        subprocess.run(["swiftc", "-O", "-o", WINLIST, os.path.join(ROOT, "tools", "emu", "winlist.swift")], check=True)
    out = subprocess.run([WINLIST, "Astris"], capture_output=True, text=True, check=True).stdout
    for line in out.splitlines():
        wid, _x, _y, _w, _h, title = line.split("\t", 5)
        if title.startswith(GAME_TITLE):
            return wid, int(_w), int(_h), int(_x), int(_y)
    raise SystemExit("no Astris game window")


# the toolbar above the game picture (measured: the letterbox bars start 104 px down a 2x window capture)
TOOLBAR_PT = 52


def grab(win, path):
    """The game picture: aspect-fit at 16:9 and centred in the window below its toolbar."""
    tmp = path + ".raw.png"
    subprocess.run(["screencapture", "-x", "-o", "-l", win[0], tmp], check=True)
    im = to_srgb(Image.open(tmp))
    os.remove(tmp)
    im.crop(crop_box(win, im.size[0])).resize((1920, 1080), Image.LANCZOS).save(path)


def to_srgb(im):
    """A window capture comes in the display's colour space (its ICC profile, Display P3 here), which
    desaturates the game's sRGB colours (bow R 27 -> 44); convert it back so pixels compare with knife's."""
    icc = im.info.get("icc_profile")
    im = im.convert("RGB")
    if not icc:
        return im
    src = ImageCms.ImageCmsProfile(io.BytesIO(icc))
    return ImageCms.profileToProfile(im, src, ImageCms.createProfile("sRGB"), outputMode="RGB")


def crop_box(win, px_w):
    """The game picture in a window image px_w pixels wide."""
    _wid, ww, wh = win[:3]
    k = px_w / ww
    aw, ah = ww, wh - TOOLBAR_PT
    gw, gh = min(aw, ah * 16 / 9), min(ah, aw * 9 / 16)
    x0, y0 = (aw - gw) / 2, TOOLBAR_PT + (ah - gh) / 2
    return tuple(round(v * k) for v in (x0, y0, x0 + gw, y0 + gh))


def rec(name, keys, fps, until):
    """Record the Astris window (ScreenCaptureKit, works while covered) while sending KEYS, keeping a frame every
    1000/FPS ms from input time 0 (the first recorded frame) as emu_NNNN.jpg."""
    d = os.path.join(OUT, name)
    os.makedirs(d, exist_ok=True)
    for f in os.listdir(d):
        if f.startswith("emu_"):
            os.remove(os.path.join(d, f))
    if not os.path.exists(WINREC):
        subprocess.run(["swiftc", "-O", "-o", WINREC, os.path.join(ROOT, "tools", "emu", "winrec.swift")], check=True)
    win = window()
    ev = parse_keys(keys)
    until = until or (ev[-1][0] + 2000 if ev else 3000)
    focus()
    ctrl("reset")
    raw = os.path.join(d, "raw")
    shutil.rmtree(raw, ignore_errors=True)
    p = subprocess.Popen([WINREC, win[0], str(fps), str(until / 1000), raw], stdout=subprocess.PIPE, text=True)
    if p.stdout.readline().strip() != "start":
        raise SystemExit("winrec did not start")
    t0 = time.monotonic()
    for ms, key in ev:
        dt = t0 + ms / 1000 - time.monotonic()
        if dt > 0:
            time.sleep(dt)
        send_key(key)
    p.wait()
    ctrl("reset")
    for f in sorted(os.listdir(raw)):
        im = to_srgb(Image.open(os.path.join(raw, f)))
        im.crop(crop_box(win, im.size[0])).resize((1920, 1080), Image.LANCZOS).save(
            os.path.join(d, "emu_" + f[2:]), quality=92)
    shutil.rmtree(raw)
    print(d, "recorded")


def click(pt, count=1):
    if not os.path.exists(CLICK):
        subprocess.run(["swiftc", "-O", "-o", CLICK, os.path.join(ROOT, "tools", "emu", "click.swift")], check=True)
    subprocess.run([CLICK, str(pt[0]), str(pt[1]), str(count)], check=True)


TOUCH = os.path.join(OUT, "touch")


def screen_pt(win, x, y):
    """Screen point (pt) of game coordinate (x, y) in 1920x1080."""
    _wid, ww, wh, wx, wy = win
    x0, y0, x1, y1 = crop_box(win, ww)
    return wx + x0 + x * (x1 - x0) / 1920, wy + y0 + y * (y1 - y0) / 1080


def touch(pts, ms):
    if not os.path.exists(TOUCH):
        subprocess.run(["swiftc", "-O", "-o", TOUCH, os.path.join(ROOT, "tools", "emu", "touch.swift")], check=True)
    win = window()
    focus()
    args = []
    for x, y in pts:
        args += [str(v) for v in screen_pt(win, x, y)]
    subprocess.run([TOUCH] + args + [str(ms)], check=True)
    # the resting pointer still counts as the Switch's mouse (left at the top it reopens the mode panel): park it
    # at the centre of the picture
    time.sleep(0.05)
    subprocess.run([TOUCH, "move"] + [str(v) for v in screen_pt(win, 960, 540)], check=True)


def astris_window(title):
    """The window's top-left (pt) from System Events: CGWindow bounds go stale for a window on another Space."""
    # one line per window, tab-separated: the title can hold commas (", General" while the game runs)
    script = ('set o to ""\ntell application "System Events" to tell process "Astris"\n'
              'repeat with w in every window\nset p to position of w\n'
              'set o to o & (name of w) & tab & (item 1 of p) & tab & (item 2 of p) & linefeed\n'
              'end repeat\nend tell\nreturn o')
    out = subprocess.run(["osascript", "-e", script], capture_output=True, text=True).stdout
    for line in out.splitlines():
        v = line.split("\t")
        if len(v) == 3 and v[0].startswith(title):
            return int(v[1]), int(v[2])
    return None


def game_running():
    """Either build's game window (the Library window turns into the game's while it runs)."""
    return any(astris_window(t) for _, t, *_ in GAMES.values())


def restart(save):
    """The running game writes its save back, so a new one goes in only while the game is stopped."""
    if not os.path.exists(WINLIST):
        subprocess.run(["swiftc", "-O", "-o", WINLIST, os.path.join(ROOT, "tools", "emu", "winlist.swift")], check=True)
    subprocess.run(["osascript", "-e", 'tell application id "V380-Ori.Astris" to activate'], check=False)
    time.sleep(1)
    if game_running():
        subprocess.run(["osascript", "-e", 'tell application "System Events" to tell process "Astris" to click '
                        'menu item "Stop" of menu "Emulation" of menu bar 1'], check=True)
        time.sleep(4)
    lib = astris_window("Library")
    if not lib:
        raise SystemExit("no Astris Library window (is the game still running?)")
    if save:
        live = live_save()
        os.makedirs(os.path.join(ROOT, SAVE_COPY), exist_ok=True)
        for dst in (os.path.join(live, "0", "savedata000"), os.path.join(live, "1", "savedata000"),
                    os.path.join(ROOT, SAVE_COPY, "savedata000")):
            shutil.copyfile(save, dst)
    # the Stop leaves Astris without focus, and the list ignores a double-click then
    subprocess.run(["osascript", "-e", 'tell application id "V380-Ori.Astris" to activate'], check=False)
    time.sleep(1)
    click((lib[0] + CLICK_GAME[0], lib[1] + CLICK_GAME[1]), 2)
    time.sleep(33)
    window()
    print("restarted")


def parse_keys(keys):
    ev = []
    for k in keys.split(","):
        if not k:
            continue
        ms, key = k.split(":", 1)
        ev.append((float(ms), key))
    return sorted(ev)


STICK = {"lup": ("L", 0, 1), "ldown": ("L", 0, -1), "lleft": ("L", -1, 0), "lright": ("L", 1, 0),
         "rup": ("R", 0, 1), "rdown": ("R", 0, -1)}


def send_key(key):
    if key == "wait":
        return
    if key.endswith("+"):
        ctrl(f"hold {key[:-1]}")
    elif key.endswith("-"):
        ctrl(f"release {key[:-1]}")
    elif key.startswith("tap/"):
        # tap/X/Y[/MS]: a touch at game coordinates (see tap)
        v = [float(n) for n in key[4:].split("/")]
        touch([(v[0], v[1])], v[2] if len(v) > 2 else 80)
    elif key in STICK:
        s, x, y = STICK[key]
        ctrl(f"stick {s} {x} {y}")
        time.sleep(0.1)
        ctrl(f"stick {s} 0 0")
    else:
        ctrl(f"press {key} 80")


def opt(args, name, default):
    if name in args:
        i = args.index(name)
        v = float(args[i + 1])
        del args[i : i + 2]
        return v
    return default


def run(name, keys, every, until):
    d = os.path.join(OUT, name)
    os.makedirs(d, exist_ok=True)
    for f in os.listdir(d):
        if f.startswith("emu_"):
            os.remove(os.path.join(d, f))
    wid = window()
    ev = parse_keys(keys)
    until = until or (ev[-1][0] + 2000 if ev else 3000)
    focus()
    ctrl("reset")
    t0 = time.monotonic()

    def inputs():
        for ms, key in ev:
            dt = t0 + ms / 1000 - time.monotonic()
            if dt > 0:
                time.sleep(dt)
            send_key(key)

    th = threading.Thread(target=inputs)
    th.start()
    n = 0
    times = []
    while True:
        at = n * every
        if at > until:
            break
        dt = t0 + at / 1000 - time.monotonic()
        if dt > 0:
            time.sleep(dt)
        t = time.monotonic() - t0
        grab(wid, os.path.join(d, f"emu_{n:04d}.png"))
        times.append(f"{n} {t * 1000:.0f} {(time.monotonic() - t0) * 1000:.0f}")
        n += 1
    th.join()
    ctrl("reset")
    with open(os.path.join(d, "emu_times.txt"), "w") as f:
        f.write("# capture, ms at start, ms at end (a window capture takes 0.25-0.5 s)\n" + "\n".join(times) + "\n")
    print(d, n, "captures")


def knife(name, keys, every, until, extra):
    d = os.path.join(OUT, name)
    os.makedirs(d, exist_ok=True)
    sv = os.path.join(OUT, "save")
    shutil.rmtree(sv, ignore_errors=True)
    os.makedirs(sv)
    shutil.copyfile(os.path.join(ROOT, SAVE_COPY, "savedata000"), os.path.join(sv, "savedata.bin"))
    shots = os.path.join(d, "knife_raw")
    shutil.rmtree(shots, ignore_errors=True)
    ev = parse_keys(keys)
    until = until or (ev[-1][0] + 2000 if ev else 3000)
    if "--title" in extra:
        extra = extra + [x for x in ("--boot", "--skip-intro") if x not in extra]
    kk = ",".join(f"{int(ms)}:{k}" for ms, k in ev if k != "wait")
    cmd = [os.path.join(ROOT, "target", "release", "knife"), "--shots", shots, "--shot-every", str(int(every)),
           "--exit-after", str(int(until + every)), "--assets-dir", os.path.join(ROOT, ROMFS), "--save-file", os.path.join(sv, "savedata.bin")] \
        + (["--keys", kk] if kk else []) + extra
    subprocess.run(cmd, cwd=ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        focus()  # back to Astris for the next emulator command
    except SystemExit:
        pass
    for f in os.listdir(d):
        if f.startswith("knife_") and f.endswith(".png"):
            os.remove(os.path.join(d, f))
    for i, f in enumerate(sorted(x for x in os.listdir(shots) if x.endswith(".png"))):
        Image.open(os.path.join(shots, f)).convert("RGB").save(os.path.join(d, f"knife_{i:04d}.png"))
    shutil.rmtree(shots)
    shutil.rmtree(sv)
    print(d)


def compare(name):
    d = os.path.join(OUT, name)
    emu = sorted(f for f in os.listdir(d) if f.startswith("emu_") and f.endswith(".png"))
    kn = sorted(f for f in os.listdir(d) if f.startswith("knife_") and f.endswith(".png"))
    n = max(len(emu), len(kn))
    tw, th = 640, 360
    sheet = Image.new("RGB", (tw * 2 + 8, (th + 4) * n), (40, 40, 40))
    for i in range(n):
        for col, lst in ((0, emu), (1, kn)):
            if i < len(lst):
                im = Image.open(os.path.join(d, lst[i])).resize((tw, th), Image.LANCZOS)
                sheet.paste(im, (col * (tw + 8), i * (th + 4)))
    out = os.path.join(d, "compare.png")
    sheet.save(out)
    print(out, n, "rows")


def main():
    a = sys.argv[1:]
    if not a:
        raise SystemExit(__doc__)
    op = a.pop(0)
    if op in ("press", "hold", "release", "stick", "reset", "state", "touch", "untouch"):
        focus()
        print(ctrl(" ".join([op] + a)))
    elif op == "shot":
        grab(window(), a[0])
    elif op == "rec":
        fps = opt(a, "--fps", 10)
        until = opt(a, "--until", 0)
        rec(a[0], a[1], fps, until)
    elif op in ("run", "knife"):
        every = opt(a, "--every", 500)
        until = opt(a, "--until", 0)
        name, keys = a[0], a[1]
        if op == "run":
            run(name, keys, every, until)
        else:
            knife(name, keys, every, until, a[2:])
    elif op == "compare":
        compare(a[0])
    elif op == "tap":
        touch([(float(a[0]), float(a[1]))], float(a[2]) if len(a) > 2 else 150)
    elif op == "drag":
        touch([(float(a[0]), float(a[1])), (float(a[2]), float(a[3]))], float(a[4]) if len(a) > 4 else 300)
    elif op == "restart":
        restart(a[0] if a else None)
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()
