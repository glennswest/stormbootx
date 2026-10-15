#!/usr/bin/env python3
"""The boot panel's screen under QEMU (#144): the monitor, and checks on a dump.

  screen.py dump SOCK OUT.ppm      screendump through the QEMU monitor socket
  screen.py key SOCK KEY           sendkey (f2, esc, ...)
  screen.py panel PPM              the panel is drawn: a light-grey header bar
                                   across the top text row
  screen.py red PPM                some red on the screen (a failed step)
  screen.py same PPM PPM           identical below the header row (its clock moves)
  screen.py differ PPM PPM         different below the header row
  screen.py png PPM                one line: the dump as a base64 PNG, so a
                                   screenshot can leave a build VM in its log
  screen.py hmp SOCK CMD...        a monitor command's output (info registers,
                                   info status): what a stuck guest is doing

Standard library only. Exit 0 when the check holds.
"""
import base64
import socket
import struct
import sys
import time
import zlib

HEADER_PX = 19  # a text row: EDK2's GraphicsConsole cell is 8x19


def monitor(sock, cmd):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(10)
    for _ in range(50):  # QEMU makes the socket a moment after it starts
        try:
            s.connect(sock)
            break
        except (FileNotFoundError, ConnectionRefusedError):
            time.sleep(0.1)
    else:
        s.connect(sock)
    time.sleep(0.3)
    s.recv(65536)  # the banner
    s.sendall((cmd + "\n").encode())
    time.sleep(1.0)
    out = b""
    try:
        out = s.recv(1 << 20)
    except socket.timeout:
        pass
    s.close()
    return out.decode(errors="replace")


def read_ppm(path):
    data = open(path, "rb").read()
    parts = []
    i = 0
    while len(parts) < 4:
        while data[i:i + 1].isspace():
            i += 1
        if data[i:i + 1] == b"#":
            while data[i:i + 1] != b"\n":
                i += 1
            continue
        j = i
        while not data[j:j + 1].isspace():
            j += 1
        parts.append(data[i:j])
        i = j
    i += 1
    if parts[0] != b"P6":
        sys.exit(f"{path}: not a P6 PPM")
    w, h = int(parts[1]), int(parts[2])
    return w, h, data[i:i + w * h * 3]


def pixels(w, px, y0, y1):
    for y in range(y0, y1):
        row = px[y * w * 3:(y + 1) * w * 3]
        for x in range(w):
            yield row[x * 3], row[x * 3 + 1], row[x * 3 + 2]


def main():
    cmd = sys.argv[1]
    if cmd == "dump":
        monitor(sys.argv[2], f"screendump {sys.argv[3]}")
        for _ in range(20):
            try:
                read_ppm(sys.argv[3])
                return 0
            except Exception:
                time.sleep(0.5)
        sys.exit("no screendump")
    if cmd == "key":
        monitor(sys.argv[2], f"sendkey {sys.argv[3]}")
        return 0
    if cmd == "hmp":
        text = monitor(sys.argv[2], " ".join(sys.argv[3:]))
        for line in text.replace("\r", "").splitlines():
            # The monitor echoes what was typed, with terminal escapes.
            if line.strip() and not line.startswith("(qemu)") and "\x1b" not in line:
                print(line)
        return 0
    if cmd == "panel":
        w, h, px = read_ppm(sys.argv[2])
        grey = sum(1 for r, g, b in pixels(w, px, 2, HEADER_PX - 2) if r == g == b and r >= 0x80)
        n = w * (HEADER_PX - 4)
        print(f"header row: {grey} of {n} pixels light grey")
        return 0 if grey * 2 > n else 1
    if cmd == "red":
        w, h, px = read_ppm(sys.argv[2])
        red = sum(1 for r, g, b in pixels(w, px, 0, h) if r >= 0xa0 and g < 0x80 and b < 0x80)
        print(f"red pixels: {red}")
        return 0 if red > 40 else 1
    if cmd in ("same", "differ"):
        wa, ha, a = read_ppm(sys.argv[2])
        wb, hb, b = read_ppm(sys.argv[3])
        if (wa, ha) != (wb, hb):
            print("the dumps differ in size")
            return 0 if cmd == "differ" else 1
        cut = wa * HEADER_PX * 3
        diff = sum(1 for x, y in zip(a[cut:], b[cut:]) if x != y)
        print(f"{diff} bytes differ below the header")
        return 0 if (diff == 0) == (cmd == "same") else 1
    if cmd == "png":
        w, h, px = read_ppm(sys.argv[2])
        raw = b"".join(b"\x00" + px[y * w * 3:(y + 1) * w * 3] for y in range(h))

        def chunk(t, d):
            c = struct.pack(">I", len(d)) + t + d
            return c + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)

        png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
               + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))
        print(base64.b64encode(png).decode())
        return 0
    sys.exit(f"unknown command {cmd}")


if __name__ == "__main__":
    sys.exit(main())
