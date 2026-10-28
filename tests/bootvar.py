#!/usr/bin/env python3
"""Add a one-shot UEFI HTTP boot from a URI to an OVMF variable store (#172).

    tests/bootvar.py VARS.fd URI

Writes Boot0099 (a short-form `Uri(URI)` device path, which EDK2's boot
manager expands over every HTTP boot LoadFile it has) and BootNext = 0x0099
into the empty space of VARS.fd's variable store, as the firmware itself
would have. That is how a test HTTP-boots without a DHCP server that answers
the `HTTPClient` class (slirp's never does): the URI comes from the boot
option and DHCP only gives the address.

EDK2's store (MdeModulePkg VariableFormat.h): a VARIABLE_STORE_HEADER whose
GUID says authenticated (60-byte variable headers) or not (32), then the
variables, each header 4-byte aligned, name and data unpadded (ALIGNMENT 1
on x86 and Arm), state VAR_ADDED (0x3f), free space 0xff. Standard library
only.
"""
import struct
import sys
import uuid

AUTH = uuid.UUID("aaf32c78-947b-439a-a180-2e144ec37792").bytes_le
PLAIN = uuid.UUID("ddcf3616-3275-4164-98b6-fe85707ffe7d").bytes_le
GLOBAL = uuid.UUID("8be4df61-93ca-11d2-aa0d-00e098032b8c").bytes_le
NV_BS_RT = 0x7


def variable(auth, name, data):
    name16 = (name + "\0").encode("utf-16-le")
    if auth:
        # StartId, State, Reserved, Attributes, MonotonicCount, TimeStamp,
        # PubKeyIndex, NameSize, DataSize, VendorGuid
        head = struct.pack("<HBBIQ16sIII16s", 0x55AA, 0x3F, 0, NV_BS_RT, 0, b"\0" * 16, 0,
                           len(name16), len(data), GLOBAL)
        assert len(head) == 60
    else:
        head = struct.pack("<HBBIII16s", 0x55AA, 0x3F, 0, NV_BS_RT, len(name16), len(data), GLOBAL)
        assert len(head) == 32
    return head + name16 + data


def main():
    path, uri = sys.argv[1], sys.argv[2]
    fd = bytearray(open(path, "rb").read())
    at = fd.find(AUTH)
    auth = at >= 0
    if not auth:
        at = fd.find(PLAIN)
    if at < 0:
        sys.exit(f"bootvar: no variable store in {path}")
    size, fmt, state = struct.unpack_from("<IBB", fd, at + 16)
    if (fmt, state) != (0x5A, 0xFE):
        sys.exit(f"bootvar: the store is not formatted and healthy ({fmt:#x}, {state:#x})")
    end = at + size
    hlen = 60 if auth else 32
    off = at + 28
    while struct.unpack_from("<H", fd, off)[0] == 0x55AA:
        if auth:
            nsz, dsz = struct.unpack_from("<II", fd, off + 36)
        else:
            nsz, dsz = struct.unpack_from("<II", fd, off + 8)
        off = (off + hlen + nsz + dsz + 3) & ~3
    u = uri.encode()
    # EFI_LOAD_OPTION: ACTIVE, the path's length, the description, then the
    # path: Uri() (messaging 0x03, subtype 0x18) and the end node.
    path_list = struct.pack("<BBH", 3, 0x18, 4 + len(u)) + u + bytes([0x7F, 0xFF, 4, 0])
    option = struct.pack("<IH", 1, len(path_list)) + "stormbootx over HTTP\0".encode("utf-16-le") + path_list
    new = variable(auth, "Boot0099", option)
    new += b"\0" * (-len(new) % 4)
    new += variable(auth, "BootNext", struct.pack("<H", 0x0099))
    if off + len(new) > end or any(b != 0xFF for b in fd[off:off + len(new)]):
        sys.exit(f"bootvar: no free space at {off:#x} in the store")
    fd[off:off + len(new)] = new
    open(path, "wb").write(fd)
    print(f"bootvar: Boot0099 Uri({uri}) and BootNext written at {off:#x} "
          f"({'authenticated' if auth else 'plain'} store at {at:#x})")


if __name__ == "__main__":
    main()
