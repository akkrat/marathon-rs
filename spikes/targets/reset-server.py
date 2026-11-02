"""reset-server.py <port>: accepts TCP connections on 127.0.0.1 and resets each one at once (row T19, "reset")."""

import socket
import struct
import sys

server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", int(sys.argv[1])))
server.listen(8)
while True:
    connection, _ = server.accept()
    # SO_LINGER with a zero timeout makes close() send RST instead of FIN
    connection.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
    connection.close()
