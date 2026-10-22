"""Capture-only fault proxy: suppress selected events, forward everything else including fds.

Run with MODE SOCKET on a LockEnv's upstream WAYLAND_DISPLAY. Object IDs are learned from
bind/create requests, never guessed. Each connection has its own object map (Mesa connects too).
No delay of registry/sync/lock/input events: the production two-second capture timeout must fire.
"""

import os
import select
import socket
import struct
import sys
import threading


def messages(buffer):
    while len(buffer) >= 8:
        oid, word = struct.unpack_from("=II", buffer)
        size = word >> 16
        if len(buffer) < size:
            break
        yield oid, word & 0xffff, buffer[:size]
        buffer = buffer[size:]


def serve(client, upstream, mode):
    server = socket.socket(socket.AF_UNIX)
    server.connect(upstream)
    objects = {}
    buffers = {client: b"", server: b""}
    pending = {client: [], server: []}
    try:
        while True:
            ready, _, _ = select.select([client, server], [], [])
            for sock in ready:
                data, fds, _, _ = socket.recv_fds(sock, 65536, 64)
                pending[sock].extend(fds)
                if not data:
                    return
                buffers[sock] += data
                out = b""
                for oid, op, msg in messages(buffers[sock]):
                    buffers[sock] = buffers[sock][len(msg):]
                    iface = objects.get(oid)
                    if sock is client:
                        if oid == 1 and op == 1:  # wl_display.get_registry
                            objects[struct.unpack_from("=I", msg, 8)[0]] = "wl_registry"
                        elif iface == "wl_registry" and op == 0:  # bind name, string, version, id
                            n = struct.unpack_from("=I", msg, 12)[0]
                            name = msg[16:16 + n - 1].decode()
                            new = struct.unpack_from("=I", msg, 16 + ((n + 3) & ~3) + 4)[0]
                            objects[new] = name
                        elif iface == "zwlr_screencopy_manager_v1" and op in (0, 1):
                            objects[struct.unpack_from("=I", msg, 8)[0]] = "wlr-frame"
                        elif iface == "ext_image_copy_capture_manager_v1" and op == 0:
                            objects[struct.unpack_from("=I", msg, 8)[0]] = "ext-session"
                        elif iface == "ext-session" and op == 0:
                            objects[struct.unpack_from("=I", msg, 8)[0]] = "ext-frame"
                    elif oid == 1 and op == 1:  # wl_display.delete_id: IDs are reusable
                        objects.pop(struct.unpack_from("=I", msg, 8)[0], None)
                    elif ((mode == "wlr" and iface == "wlr-frame")
                          or (mode == "ext-session" and iface == "ext-session")
                          or (mode == "ext-frame" and iface == "ext-frame")):
                        # These interfaces' events carry no descriptors. Leave fd-bearing
                        # messages (wl_shm, keyboard keymap, EGL) untouched.
                        print(f"suppressed {iface}#{oid} event {op}", flush=True)
                        continue
                    out += msg
                if out:
                    dest = server if sock is client else client
                    # send_fds/sendmsg can short-write; ancillary data accompanies only the
                    # first byte. Finish the byte stream without sending the descriptors twice.
                    sent = socket.send_fds(dest, [out], pending[sock])
                    dest.sendall(out[sent:])
                    for fd in pending[sock]:
                        os.close(fd)
                    pending[sock].clear()
    except (OSError, ConnectionError):
        pass
    finally:
        for fds in pending.values():
            for fd in fds:
                os.close(fd)
        client.close()
        server.close()


def main():
    mode, name = sys.argv[1:]
    runtime = os.environ["XDG_RUNTIME_DIR"]
    upstream = os.path.join(runtime, os.environ["WAYLAND_DISPLAY"])
    with socket.socket(socket.AF_UNIX) as listener:
        listener.bind(os.path.join(runtime, name))
        listener.listen(4)
        print("ready", flush=True)
        while True:
            client, _ = listener.accept()
            threading.Thread(target=serve, args=(client, upstream, mode), daemon=True).start()


if __name__ == "__main__":
    main()
