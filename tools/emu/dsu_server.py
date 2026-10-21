#!/usr/bin/env python3
"""DSU (cemuhook) controller server for driving a Switch emulator (Astris) from scripts.

The emulator is the DSU client: it asks 127.0.0.1:26760 for its controller slots and pad data. This server
offers one connected pad in slot 0 and streams its state at 250 Hz (and on every change). Scripts change the
state through a control socket (TCP 127.0.0.1:26761, one text command per line, `ok` / `err ...` reply):

    press BTN [ms]     tap a button (default 80 ms)
    hold BTN | release BTN
    stick L|R X Y      stick tilt, -1..1 (+ right / up); `stick L 0 0` recentres
    touch X Y | untouch    touch point in 1920x1080 (sent as the DSU touch pad)
    reset              everything released
    state              current state as JSON

Buttons (Switch names, Nintendo positions): a b x y l r zl zr plus minus home capture lstick rstick up down
left right. DSU carries a DS4 layout, which Astris reads by Xbox position: A = cross (bottom), B = circle
(right), X = square (left), Y = triangle (top); L / R = L1 / R1, ZL / ZR = L2 / R2, plus = options,
minus = share, home = PS.
"""

import json
import socket
import struct
import threading
import time
import zlib

DSU_PORT = 26760
CTRL_PORT = 26761
VERSION = 1001
SERVER_ID = 0x4B4E4946  # "KNIF"
MAC = bytes([0x00, 0x00, 0x4B, 0x4E, 0x49, 0x46])

MSG_VERSION = 0x100000
MSG_PORTS = 0x100001
MSG_DATA = 0x100002

# (byte, bit) in buttons1 / buttons2, and the analog byte index (dpad l/d/r/u, square, cross, circle,
# triangle, r1, l1, r2, l2)
B1 = {"minus": 0x01, "lstick": 0x02, "rstick": 0x04, "plus": 0x08, "up": 0x10, "right": 0x20, "down": 0x40, "left": 0x80}
B2 = {"zl": 0x01, "zr": 0x02, "l": 0x04, "r": 0x08, "y": 0x10, "b": 0x20, "a": 0x40, "x": 0x80}
ANALOG = ["left", "down", "right", "up", "x", "a", "b", "y", "r", "l", "zr", "zl"]
BUTTONS = set(B1) | set(B2) | {"home", "capture"}


class Pad:
    def __init__(self):
        self.lock = threading.Lock()
        self.held = set()
        self.sticks = {"L": (0.0, 0.0), "R": (0.0, 0.0)}
        self.touch = None
        self.counter = 0
        self.changed = threading.Event()

    def snapshot(self):
        with self.lock:
            return set(self.held), dict(self.sticks), self.touch


def header(payload):
    """DSUS header around `payload` (message type + body), CRC32 over the whole packet."""
    pkt = bytearray(b"DSUS" + struct.pack("<HHII", VERSION, len(payload), 0, SERVER_ID) + payload)
    struct.pack_into("<I", pkt, 8, zlib.crc32(pkt) & 0xFFFFFFFF)
    return bytes(pkt)


def slot_info(slot, connected):
    # slot, state (2 connected), model (2 full gyro), connection (1 usb), MAC, battery (5 full)
    if not connected:
        return struct.pack("<BBBB", slot, 0, 0, 0) + bytes(6) + b"\x00"
    return struct.pack("<BBBB", slot, 2, 2, 1) + MAC + b"\x05"


def axis(v):
    return max(0, min(255, round(128 + v * 127)))


def pad_packet(pad):
    held, sticks, touch = pad.snapshot()
    pad.counter += 1
    b1 = sum(bit for n, bit in B1.items() if n in held)
    b2 = sum(bit for n, bit in B2.items() if n in held)
    lx, ly = sticks["L"]
    rx, ry = sticks["R"]
    body = slot_info(0, True) + struct.pack("<BI", 1, pad.counter)
    body += struct.pack("<BBBB", b1, b2, int("home" in held), int("capture" in held))
    body += bytes([axis(lx), axis(ly), axis(rx), axis(ry)])
    body += bytes(255 if n in held else 0 for n in ANALOG)
    if touch:
        x, y = touch
        body += struct.pack("<BBHH", 1, 0, x, y)
    else:
        body += struct.pack("<BBHH", 0, 0, 0, 0)
    body += struct.pack("<BBHH", 0, 0, 0, 0)
    body += struct.pack("<Q", int(time.time() * 1e6))
    # accelerometer (g; resting flat: -1 on y as DS4 servers send) and gyro (deg/s)
    body += struct.pack("<ffffff", 0.0, -1.0, 0.0, 0.0, 0.0, 0.0)
    return header(struct.pack("<I", MSG_DATA) + body)


def dsu_loop(pad, log):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("127.0.0.1", DSU_PORT))
    clients = {}  # addr -> last request time
    lock = threading.Lock()

    def sender():
        while True:
            pad.changed.wait(0.004)
            pad.changed.clear()
            now = time.time()
            with lock:
                live = [a for a, t in clients.items() if now - t < 6.0]
                for a in list(clients):
                    if a not in live:
                        del clients[a]
            if live:
                pkt = pad_packet(pad)
                for a in live:
                    try:
                        sock.sendto(pkt, a)
                    except OSError:
                        pass

    threading.Thread(target=sender, daemon=True).start()
    while True:
        data, addr = sock.recvfrom(1024)
        if len(data) < 20 or data[:4] != b"DSUC":
            continue
        (msg,) = struct.unpack_from("<I", data, 16)
        if msg == MSG_VERSION:
            sock.sendto(header(struct.pack("<IH", MSG_VERSION, VERSION)), addr)
        elif msg == MSG_PORTS:
            (n,) = struct.unpack_from("<i", data, 20)
            for slot in data[24 : 24 + max(0, min(n, 4))]:
                sock.sendto(header(struct.pack("<I", MSG_PORTS) + slot_info(slot, slot == 0) + b"\x00"), addr)
        elif msg == MSG_DATA:
            with lock:
                if addr not in clients:
                    log(f"client {addr[0]}:{addr[1]} subscribed")
                clients[addr] = time.time()


def ctrl_loop(pad, log):
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", CTRL_PORT))
    srv.listen(4)

    def handle(conn):
        f = conn.makefile("rw")
        for line in f:
            try:
                reply = command(pad, line.split())
            except Exception as e:  # noqa: BLE001 - report to the client
                reply = f"err {e}"
            f.write(reply + "\n")
            f.flush()
        conn.close()

    while True:
        conn, _ = srv.accept()
        threading.Thread(target=handle, args=(conn,), daemon=True).start()


def button(name):
    n = name.lower()
    if n not in BUTTONS:
        raise ValueError(f"unknown button {name}")
    return n


def command(pad, w):
    if not w:
        return "ok"
    op = w[0].lower()
    if op == "press":
        b = button(w[1])
        ms = float(w[2]) if len(w) > 2 else 80.0
        with pad.lock:
            pad.held.add(b)
        pad.changed.set()
        time.sleep(ms / 1000)
        with pad.lock:
            pad.held.discard(b)
        pad.changed.set()
    elif op == "hold":
        with pad.lock:
            pad.held.add(button(w[1]))
        pad.changed.set()
    elif op == "release":
        with pad.lock:
            pad.held.discard(button(w[1]))
        pad.changed.set()
    elif op == "stick":
        with pad.lock:
            pad.sticks[w[1].upper()] = (float(w[2]), float(w[3]))
        pad.changed.set()
    elif op == "touch":
        with pad.lock:
            pad.touch = (int(float(w[1])), int(float(w[2])))
        pad.changed.set()
    elif op == "untouch":
        with pad.lock:
            pad.touch = None
        pad.changed.set()
    elif op == "reset":
        with pad.lock:
            pad.held.clear()
            pad.sticks = {"L": (0.0, 0.0), "R": (0.0, 0.0)}
            pad.touch = None
        pad.changed.set()
    elif op == "state":
        held, sticks, touch = pad.snapshot()
        return "ok " + json.dumps({"held": sorted(held), "sticks": sticks, "touch": touch})
    else:
        raise ValueError(f"unknown command {op}")
    return "ok"


def main():
    pad = Pad()

    def log(m):
        print(time.strftime("%H:%M:%S"), m, flush=True)

    threading.Thread(target=ctrl_loop, args=(pad, log), daemon=True).start()
    log(f"DSU server on 127.0.0.1:{DSU_PORT}, control on 127.0.0.1:{CTRL_PORT}")
    dsu_loop(pad, log)


if __name__ == "__main__":
    main()
