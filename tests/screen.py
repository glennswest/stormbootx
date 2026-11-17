#!/usr/bin/env python3
"""The boot panel's screen under QEMU (#144): the monitor, and checks on a dump.

  screen.py dump SOCK OUT.ppm      screendump through the QEMU monitor socket
  screen.py key SOCK KEY           sendkey (f2, esc, ...)
  screen.py panel PPM              the panel is drawn: the spec's frame (#220,
                                   stormblock-screen-spec), its header bar
                                   amber (or red, failed) and the lower
                                   frame's blocks blue, amber and peach, at
                                   their places on the 1920x1080 grid
  screen.py frame PPM              the same, said as the line `handover : …`
                                   (net-ovmf: the frame left up under the
                                   bootloader)
  screen.py red PPM                some red on the screen (a failed step)
  screen.py same PPM PPM           identical below the header frame (its clock moves)
  screen.py differ PPM PPM         different below the header frame
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

# stormblock-screen-spec's palette and frame (stormblock#637, pinned in
# Cargo.toml): design pixels on a 1920x1080 grid, scaled across and down.
AMBER = (255, 153, 0)
RED = (221, 68, 68)
BLUE = (153, 204, 255)
PEACH = (255, 204, 153)
GREY = (170, 160, 150)
HEADER_H = 348  # the lower frame's top: the header frame and its clock above
# (design x, y, the colours it may be, what it is)
FRAME = [
    (600, 46, (AMBER, RED), "the header bar"),
    (124, 263, (GREY,), "the header's release block"),
    (124, 512, (BLUE,), "the lower frame's first block"),
    (124, 775, (AMBER,), "its second"),
    (124, 928, (PEACH,), "its third"),
]


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
    if cmd in ("panel", "frame"):
        w, h, px = read_ppm(sys.argv[2])
        bad = []
        for dx, dy, want, what in FRAME:
            x, y = dx * w // 1920, dy * h // 1080
            # A 5x5 patch, every pixel the colour: fills are exact.
            got = {tuple(px[(yy * w + xx) * 3:(yy * w + xx) * 3 + 3]) for yy in range(y - 2, y + 3) for xx in range(x - 2, x + 3)}
            if len(got) != 1 or next(iter(got)) not in want:
                bad.append(f"{what} at ({x},{y}) is {sorted(got)[:3]}")
        head = "red" if tuple(px[(46 * h // 1080 * w + 600 * w // 1920) * 3:][:3]) == RED else "amber"
        if cmd == "frame":
            print(f"handover    : " + (f"the spec's frame stays up ({head} header, {w}x{h})" if not bad else "no frame: " + "; ".join(bad)))
        else:
            print(f"frame on {w}x{h}: " + ("the spec's, header " + head if not bad else "; ".join(bad)))
        return 0 if not bad else 1
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
        cut = wa * (HEADER_H * ha // 1080) * 3
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
