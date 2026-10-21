#!/usr/bin/env python3
# A GPT disk at 4096-byte blocks with one ESP partition holding ESP.IMG
# (a FAT image), the shape stormblock lays on a 4K release.
#
#   tests/gpt4k.py ESP.IMG DISK.IMG
#
# Used by tests/net-ovmf.sh (the stub target's disk) and tests/laptop-ovmf.sh
# (the image stormboot-serve serves, #164).
import struct, sys, uuid, zlib
esp, out = sys.argv[1], sys.argv[2]
bs = 4096
data = open(esp, 'rb').read()
first = (1 << 20) // bs
last = first + -(-len(data) // bs) - 1
total = last + 1 + 1 + 4 + 256
entries = bytearray(128 * 128)
entries[0:128] = (uuid.UUID('C12A7328-F81F-11D2-BA4B-00A0C93EC93B').bytes_le
                  + uuid.uuid4().bytes_le + struct.pack('<QQQ', first, last, 0)
                  + 'EFI system'.encode('utf-16-le').ljust(72, b'\0'))
ecrc = zlib.crc32(entries)
disk_guid = uuid.uuid4().bytes_le
def header(me, alt, table):
    h = struct.pack('<8sIIIIQQQQ16sQIII', b'EFI PART', 0x10000, 92, 0, 0,
                    me, alt, 6, total - 6, disk_guid, table, 128, 128, ecrc)
    return h[:16] + struct.pack('<I', zlib.crc32(h)) + h[20:]
img = bytearray(total * bs)
img[446:462] = struct.pack('<BBBBBBBBII', 0, 0, 2, 0, 0xEE, 0xFF, 0xFF, 0xFF, 1,
                           min(total - 1, 0xFFFFFFFF))
img[510:512] = b'\x55\xaa'
img[bs:bs + 92] = header(1, total - 1, 2)
img[2 * bs:2 * bs + len(entries)] = entries
img[first * bs:first * bs + len(data)] = data
img[(total - 5) * bs:(total - 5) * bs + len(entries)] = entries
img[(total - 1) * bs:(total - 1) * bs + 92] = header(total - 1, 1, total - 5)
open(out, 'wb').write(img)
