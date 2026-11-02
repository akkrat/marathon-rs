"""kex-fail-server.py <port> [reason]: an SSH server on 127.0.0.1 whose KEXINIT shares no algorithm with any client, so
the key exchange fails (row T26, "failed kex"). It sends its banner and KEXINIT, then waits for the client to give up.
With a reason code it sends SSH_MSG_DISCONNECT with that reason instead of the KEXINIT, for the connect-time
classification of disconnect reasons."""

import os
import socket
import struct
import sys


def name_list(names):
    data = ",".join(names).encode()
    return struct.pack(">I", len(data)) + data


def packet(payload):
    padding = 8 - (len(payload) + 5) % 8
    if padding < 4:
        padding += 8
    return struct.pack(">IB", len(payload) + padding + 1, padding) + payload + os.urandom(padding)


kexinit = bytes([20]) + os.urandom(16)
kexinit += name_list(["spike-no-such-kex"]) + name_list(["spike-no-such-hostkey"])
kexinit += name_list(["spike-no-such-cipher"]) * 2 + name_list(["spike-no-such-mac"]) * 2
kexinit += name_list(["none"]) * 2 + name_list([]) * 2 + bytes([0]) + struct.pack(">I", 0)

reason = int(sys.argv[2]) if len(sys.argv) > 2 else None
description = b"spike disconnect"
disconnect = bytes([1]) + struct.pack(">I", reason or 0) + struct.pack(">I", len(description)) + description
disconnect += struct.pack(">I", 0)

server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", int(sys.argv[1])))
server.listen(8)
while True:
    connection, _ = server.accept()
    try:
        connection.sendall(b"SSH-2.0-SpikeKexFail\r\n" + packet(disconnect if reason is not None else kexinit))
        connection.settimeout(10)
        while connection.recv(4096):
            pass
    except OSError:
        pass
    finally:
        connection.close()
