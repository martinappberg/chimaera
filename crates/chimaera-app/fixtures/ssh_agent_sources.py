#!/usr/bin/env python3
"""Real private Session versus duplicate external key; keeper protocol simulated.

Imports unchanged production selection/backend/verifier through the existing
Mac-only fixture binary. Never uses a personal agent, config, key or trust file.
"""
import argparse
import base64
import hashlib
import json
import os
import pathlib
import select
import signal
import socket
import socketserver
import stat
import struct
import subprocess
import sys
import threading
import time

from ssh_agent_loader import Refused, stop_owned
from ssh_agent_route import BOOT, GRANT, GRANT_PATH, TOKEN, Handler, Protocol, RouteFixture

DESTINATIONS = [
    {"hostname": "hop.example.invalid", "user": "fixture", "port": 22},
    {"hostname": "target.example.invalid", "user": "fixture", "port": 22},
]


def exact(socket_, count):
    data = bytearray()
    while len(data) < count:
        chunk = socket_.recv(count - len(data))
        if not chunk:
            raise Refused("partial fixture frame")
        data.extend(chunk)
    return bytes(data)


def agent_frame(socket_):
    size = struct.unpack("!I", exact(socket_, 4))[0]
    if not 1 <= size <= 65536:
        raise Refused("agent frame bound")
    return exact(socket_, size)


class Proxy(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = False
    block_on_close = True

    def __init__(self, owner, path, upstream, process, refuse):
        self.thread = None
        self.started = self.closed = False
        owner.proxies.append(self)  # Before bind, chmod or thread effects.
        self.stopped = threading.Event()
        self.lock = threading.Lock()
        self.slots = threading.BoundedSemaphore(8)
        self.active = set()
        self.frames = 0
        self.binds = self.signs = 0
        self.failed = False
        self.upstream, self.process, self.refuse = upstream, process, refuse
        self.upstream_identity = self.identity(upstream)
        self.deadline = owner.end(20)
        super().__init__(str(path), ProxyHandler)
        self.path = path
        self.socket_identity = self.identity(path)
        os.chmod(path, 0o600)
        self.thread = threading.Thread(target=self.serve_forever, kwargs={"poll_interval": 0.025})

    @staticmethod
    def identity(path):
        info = path.lstat()
        if not stat.S_ISSOCK(info.st_mode) or info.st_uid != os.getuid():
            raise Refused("synthetic agent socket ownership")
        return info.st_dev, info.st_ino, info.st_uid

    start = Protocol.start

    def process_request(self, request, client_address):
        if self.stopped.is_set() or not self.slots.acquire(blocking=False):
            self.failed = True
            self.shutdown_request(request)
            return
        with self.lock:
            self.active.add(request)
        try:
            super().process_request(request, client_address)
        except BaseException:
            with self.lock:
                self.active.discard(request)
            self.slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            with self.lock:
                self.active.discard(request)
            self.slots.release()

    def handle_error(self, request, client_address):
        if not self.stopped.is_set():
            self.failed = True  # No raw packets, paths or tracebacks.

    def counts(self):
        with self.lock:
            return self.binds, self.signs

    def close(self):
        if self.closed:
            return
        stopped = getattr(self, "stopped", None)
        if stopped is not None:
            stopped.set()
        active = getattr(self, "active", None)
        if active is not None:
            with self.lock:
                for connection in list(active):
                    try:
                        connection.shutdown(socket.SHUT_RDWR)
                    except OSError:
                        pass
        if self.started and self.thread.is_alive():
            self.shutdown()
        if hasattr(self, "socket"):
            self.server_close()
        if self.started:
            self.thread.join(timeout=1)
            if self.thread.is_alive():
                raise Refused("synthetic proxy thread cleanup")
        path = getattr(self, "path", None)
        if path is not None and path.exists():
            if self.identity(path) != self.socket_identity:
                raise Refused("synthetic proxy socket changed")
            path.unlink()
        self.closed = True


class ProxyHandler(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(1)
        if self.server.process.poll() is not None or self.server.identity(self.server.upstream) != self.server.upstream_identity:
            raise Refused("synthetic upstream changed")
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as upstream:
            upstream.settimeout(1)
            upstream.connect(str(self.server.upstream))
            while not self.server.stopped.is_set() and time.monotonic() < self.server.deadline:
                if not select.select([self.request], [], [], 0.025)[0]:
                    continue
                # Graceful native socket retirement is normal and emits no frame.
                if not self.request.recv(1, socket.MSG_PEEK):
                    return
                packet = agent_frame(self.request)
                if packet[0] not in (11, 13, 27):
                    raise Refused("unexpected external agent request")
                with self.server.lock:
                    self.server.frames += 1
                    if self.server.frames > 64:
                        raise Refused("external frame count bound")
                    self.server.binds += packet[0] == 27
                    self.server.signs += packet[0] == 13
                if packet[0] == 13 and self.server.refuse:
                    reply = b"\x05"  # Genuine agent FAILURE; never a fabricated signature.
                else:
                    upstream.sendall(struct.pack("!I", len(packet)) + packet)
                    reply = agent_frame(upstream)
                self.request.sendall(struct.pack("!I", len(reply)) + reply)


class SourceProtocol(Protocol):
    def __init__(self, owner, action, artifact, proxy):
        self.artifact, self.proxy = artifact, proxy
        self.finished = threading.Event()
        self.reconnect_entered = threading.Event()
        self.signatures = 0
        self.private_counts = None
        super().__init__(owner, action, artifact["public"], artifact["hosts"][0])
        self.RequestHandlerClass = SourceHandler
        self.deadline = owner.end(20)


class SourceHandler(Handler):
    def receive(self):
        head = exact(self.connection, 2)
        if head[0] != 0x81 or not head[1] & 128:
            raise Refused("source reply frame type")
        size = head[1] & 127
        if size == 126:
            size = struct.unpack("!H", exact(self.connection, 2))[0]
        elif size == 127:
            raise Refused("source reply frame bound")
        if not 1 <= size <= 65535:
            raise Refused("source reply frame bound")
        mask = exact(self.connection, 4)
        raw = exact(self.connection, size)
        reply = json.loads(bytes(byte ^ mask[i % 4] for i, byte in enumerate(raw)))
        if not isinstance(reply, dict):
            raise Refused("source reply object")
        return reply

    def do_GET(self):
        if self.path != GRANT_PATH + "/" + GRANT + "/ws":
            return super().do_GET()
        if not self.allowed() or self.server.grants != 1:
            return self.reply(401)
        key = self.headers.get("Sec-WebSocket-Key", "")
        if (self.headers.get("Upgrade", "").lower() != "websocket" or len(key) != 24
                or len(base64.b64decode(key, validate=True)) != 16):
            return self.reply(400)
        self.send_response(101)
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", base64.b64encode(hashlib.sha1(
            (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode())
        self.end_headers()
        self.close_connection = True
        with self.server.lock:
            self.frame(1, json.dumps({"type": "ready", "version": 1, "grant_id": GRANT,
                                    "keeper_boot": BOOT, "legs": 2}).encode())
            self.server.ready += 1
        # Signing begins only once the exact original reconnect request is held.
        # This is positive ordering evidence, not a timer-based scheduling guess.
        while not self.server.reconnect_entered.wait(0.025):
            if self.server.stopped.is_set() or time.monotonic() >= self.server.deadline:
                raise Refused("original source reconnect absent")
        # Each next request waits for the previous real verifier reply. It never
        # races private/external requests or counts an unverified agent signature.
        for index, request in enumerate(self.server.artifact["requests"]):
            self.frame(1, json.dumps(request, separators=(",", ":")).encode())
            reply = self.receive()
            kind = "bound" if index % 2 == 0 else "signature"
            if index == 3 and self.server.action == "refuse":
                kind = "failure"
            if (reply.get("type") != kind or reply.get("leg") != request["leg"]
                    or reply.get("request_id") != request["request_id"]
                    or reply.get("connection_id") != request["connection_id"]):
                raise Refused("source reply correlation or fallback")
            if kind == "signature":
                packet = base64.b64decode(reply.get("packet", ""), validate=True)
                if not packet or packet[0] != 14:
                    raise Refused("checked signature frame")
                self.server.signatures += 1
            if index == 1:
                self.server.private_counts = self.server.proxy.counts()
                if self.server.private_counts != (0, 0):
                    raise Refused("private leg touched external backend")
        if self.server.proxy.counts() != (1, 1):
            raise Refused("external sign retry or absent source")
        if self.server.signatures != (2 if self.server.action == "accept" else 1):
            raise Refused("duplicate source fallback")
        self.server.finished.set()
        # Retain channel until Connect settles. On refusal no second reply may
        # appear; native closes immediately and the exact grant DELETE follows.
        while not self.server.stopped.is_set() and time.monotonic() < self.server.deadline:
            if not select.select([self.connection], [], [], 0.025)[0]:
                continue
            first = self.connection.recv(1, socket.MSG_PEEK)
            if not first:
                return
            if first == b"\x81":
                raise Refused("unexpected signature after source refusal")
            return  # Native close/control, no additional scripted requests.

    def do_POST(self):
        if not self.allowed():
            return self.reply(401)
        if self.path == "/v1/hosts/fixture-host/reconnect":
            with self.server.lock:
                if (self.server.ready != 1 or self.server.grants != 1
                        or self.headers.get("x-chimaera-ssh-auth-route-grant") != GRANT):
                    return self.reply(409)
                self.server.reconnects += 1
                if self.server.reconnects != 1:
                    return self.reply(409)
                self.server.reconnect_entered.set()
            while not self.server.stopped.is_set() and time.monotonic() < self.server.deadline:
                if self.server.finished.wait(0.025):
                    if self.server.action == "accept":
                        return self.reply(204)
                    # Refused signing must end the native owner, not invoke finish.
                    self.wait()
                    return
            return
        if self.path != GRANT_PATH:
            return self.reply(404)
        size = self.headers.get("Content-Length", "")
        if not size.isdecimal() or not 1 <= int(size) <= 131072:
            return self.reply(400)
        body = self.rfile.read(int(size))
        if len(body) != int(size):
            return self.reply(400)
        request = json.loads(body)
        legs = request.get("legs", [])
        if (request.get("version") != 1 or request.get("keeper_boot") != BOOT
                or request.get("destination") != DESTINATIONS[1]
                or request.get("route") != {"version": 1, "jumps": [DESTINATIONS[0]]}
                or len(legs) != 2 or self.server.proxy.counts() != (0, 0)):
            raise Refused("source grant shape")
        for index, leg in enumerate(legs):
            if (leg.get("destination") != DESTINATIONS[index] or leg.get("mode") != "key"
                    or leg.get("user_keys") != [self.server.public]
                    or leg.get("host_keys") != [{"key": self.server.artifact["hosts"][index], "is_ca": False}]
                    or not isinstance(leg.get("policy"), dict)):
                raise Refused("source grant immutable identity")
        with self.server.lock:
            self.server.grants += 1
            if self.server.grants != 1:
                raise Refused("source grant replay")
        return self.reply(201, {"version": 1, "grant_id": GRANT, "expires_in": 180,
            "destination": request["destination"], "route": request["route"],
            "modes": [leg["mode"] for leg in legs], "policies": [leg["policy"] for leg in legs]})


class Sources(RouteFixture):
    def __init__(self, binary):
        self.proxies = []
        super().__init__(binary)
        self.deadline = time.monotonic() + 45

    def close(self):
        failed = False
        for proxy in reversed(self.proxies):
            try:
                proxy.close()
            except (Refused, OSError):
                failed = True
        try:
            super().close()
        finally:
            if failed:
                raise Refused("retained source proxy cleanup")

    def run(self):
        encrypted, public = self.key("selected")
        # Only this synthetic copy is decrypted for the separate fixture agent.
        # The captured configured file remains encrypted and has no .pub sidecar.
        duplicate = self.root / "external-duplicate"
        duplicate.write_bytes(encrypted.read_bytes())
        os.chmod(duplicate, 0o600)
        if self.collect(["/usr/bin/ssh-keygen", "-q", "-p", "-P", "fixture-only-passphrase",
                         "-N", "", "-f", str(duplicate)])[0]:
            raise Refused("synthetic duplicate preparation")
        upstream = self.root / "external-agent"
        agent = self.spawn(["/usr/bin/ssh-agent", "-D", "-P", "", "-a", str(upstream)], output=False)
        until = self.end(2)
        while not upstream.exists() and time.monotonic() < until:
            time.sleep(0.02)
        external_env = dict(self.env, SSH_AUTH_SOCK=str(upstream), SSH_ASKPASS_REQUIRE="never")
        if self.collect(["/usr/bin/ssh-add", str(duplicate)], external_env)[0]:
            raise Refused("duplicate external seed")
        external_before = self.identities(external_env)
        foreign, _ = self.key("foreign", passphrase="")
        foreign_path = self.root / "foreign-agent"
        self.spawn(["/usr/bin/ssh-agent", "-D", "-P", "", "-a", str(foreign_path)], output=False)
        until = self.end(2)
        while not foreign_path.exists() and time.monotonic() < until:
            time.sleep(0.02)
        foreign_env = dict(self.env, SSH_AUTH_SOCK=str(foreign_path), SSH_ASKPASS_REQUIRE="never")
        if self.collect(["/usr/bin/ssh-add", str(foreign)], foreign_env)[0]:
            raise Refused("foreign seed")
        foreign_before = self.identities(foreign_env)
        user = self.root / "source-user-public"
        user.write_bytes(public)
        os.chmod(user, 0o600)
        if self.collect([str(self.binary), "--source-packets", str(self.root)])[0]:
            raise Refused("public source packet preparation")
        with (self.root / "source-packets.json").open("rb") as source:
            raw = source.read(16385)
        if len(raw) > 16384:
            raise Refused("public source artifact bound")
        artifact = json.loads(raw)
        if (artifact.get("version") != 1 or artifact.get("public") != public.decode()
                or len(artifact.get("hosts", [])) != 2 or len(artifact.get("requests", [])) != 4):
            raise Refused("public source artifact shape")
        known = self.root / "known"
        known.write_text("".join(destination["hostname"] + " ssh-ed25519 " + host + "\n"
                                 for destination, host in zip(DESTINATIONS, artifact["hosts"])))
        os.chmod(known, 0o600)
        config = self.root / "config"
        for action in ("accept", "refuse"):
            proxy = Proxy(self, self.root / ("proxy-" + action), upstream, agent, action == "refuse")
            proxy.start()
            helper = SourceProtocol(self, action, artifact, proxy)
            helper.start()
            # Target's real effective IdentitiesOnly=no keeps the duplicate
            # external key AND captured file, so its backend contains both.
            config.write_text("Host source-fixture\n HostName target.example.invalid\n ProxyJump source-hop\n"
                " IdentityAgent " + str(proxy.path) + "\n IdentitiesOnly no\n"
                "Host source-hop\n HostName hop.example.invalid\n IdentityAgent none\n IdentitiesOnly yes\n"
                "Host *\n User fixture\n Port 22\n IdentityFile " + str(encrypted) + "\n"
                " UserKnownHostsFile " + str(known) + "\n GlobalKnownHostsFile none\n"
                " PubkeyAuthentication yes\n PasswordAuthentication no\n KbdInteractiveAuthentication no\n"
                " GSSAPIAuthentication no\n HostbasedAuthentication no\n PreferredAuthentications publickey\n"
                " HostKeyAlgorithms ssh-ed25519\n PubkeyAcceptedAlgorithms ssh-ed25519\n"
                " CASignatureAlgorithms ssh-ed25519\n ControlMaster no\n ControlPath none\n")
            os.chmod(config, 0o600)
            app = None
            receipts = []
            answered = 0
            output = bytearray()
            try:
                app = self.spawn([str(self.binary), "--source-fixture", str(config), helper.origin, action],
                                 self.env, input_pipe=True)
                until = self.end(18)
                while True:
                    if time.monotonic() >= until:
                        raise Refused("source case deadline")
                    if not select.select([app.stdout], [], [], 0.025)[0]:
                        continue
                    chunk = os.read(app.stdout.fileno(), 4096)
                    if not chunk:
                        break
                    if len(output) + len(chunk) > 8192:
                        raise Refused("source stdout bound")
                    output.extend(chunk)
                    count = output.count(b"SOURCE_PROMPT\n")
                    if count > 2:
                        raise Refused("source prompt count")
                    while answered < count:
                        receipts.append(self.helper_receipt(app))
                        if os.write(app.stdin.fileno(), b"CONTINUE\n") != 9:
                            raise Refused("source prompt handoff")
                        answered += 1
                        if answered == 2:
                            app.stdin.close()
                app.wait(timeout=max(0.01, until - time.monotonic()))
                expected = b"SOURCE_ACCEPTED\n" if action == "accept" else b"SOURCE_REFUSED external\n"
                if (app.returncode != 0 or expected not in output or answered != 2
                        or helper.failed or proxy.failed or not helper.finished.is_set()
                        or helper.private_counts != (0, 0) or proxy.counts() != (1, 1)
                        or (helper.grants, helper.ready, helper.reconnects, helper.deletes) != (1, 1, 1, 1)):
                    raise Refused("source mapping or cleanup receipt")
                for receipt in receipts:
                    self.cleanup_receipt(receipt)
                if self.identities(external_env) != external_before or self.identities(foreign_env) != foreign_before:
                    raise Refused("synthetic agent identities changed")
                print("PASS", action, "private_external_signs0 external_signs1 checked_signature_cleanup", flush=True)
            finally:
                if app is not None and not app.stdin.closed:
                    app.stdin.close()
                try:
                    helper.close()
                    proxy.close()
                finally:
                    if app is not None:
                        stop_owned(app)
        print("PASS source2; real Session/agents, simulated keeper, no sshd/Tauri/job acceptance", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    args = parser.parse_args()
    if sys.platform != "darwin" or not args.binary.is_absolute():
        raise Refused("Mac fixture and absolute binary required")
    info = args.binary.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
        raise Refused("fixture binary ownership")

    def interrupted(*_):
        raise Refused("source fixture interrupted or expired")

    for name in (signal.SIGALRM, signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(name, interrupted)
    signal.setitimer(signal.ITIMER_REAL, 45)
    fixture = Sources(args.binary)
    try:
        fixture.run()
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        for name in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            signal.signal(name, signal.SIG_IGN)
        fixture.close()


if __name__ == "__main__":
    try:
        main()
    except (Refused, OSError, ValueError, subprocess.TimeoutExpired) as error:
        print("FAIL", str(error) if isinstance(error, Refused) else "fixture operation refused", file=sys.stderr)
        sys.exit(1)
