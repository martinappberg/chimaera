#!/usr/bin/env python3
"""Mac production Session -> actual Linux keeper/two localhost sshd key legs.

Only account replies, Slurm discovery and native passphrase UI are synthetic.
No jobs are submitted. Three immediate existing-master reads fit normal idle
retirement; this is not a held-job/compute-forward or Tauri UI certificate.
Inner cleanup proves captured mux and original agent-directory absence. The
fixed outer wrapper must separately prove its original cgroup empty/removed,
including descendants that escape process groups. Runtime proof is still pending.
The fixed task VM management transport does not carry user SSH credentials.
"""
import argparse
import base64
import json
import os
import pathlib
import select
import shlex
import signal
import socket
import socketserver
import stat
import struct
import sys
import threading
import time

from ssh_agent_loader import Refused, stop_owned
from ssh_agent_sources import Sources, Proxy, agent_frame
from ssh_agent_route import Protocol

# Explicit fixture literals only. Exception/source/child text is never forwarded.
KNOWN_REFUSALS = frozenset({
    'Linux fixture management check failed',
    'Linux fixture immutable digest',
    'fixed command deadline',
    'fixed command output bound',
    'fixture process budget',
    'owned session receipt',
    'synthetic key generation',
    'synthetic public identity',
    'synthetic duplicate preparation',
    'external seed',
    'foreign seed',
    'foreign identity receipt',
    'fixed binary ownership',
    'Mac fixture/exact Linux binary and wrapper digests required',
    'keeper fixture interrupted or expired',
    'keeper fixture cleanup uncertain',
    'fixture root identity changed',
})


def refusal_phase(error):
    if isinstance(error, Refused):
        if len(error.args) == 1 and type(error.args[0]) is str and error.args[0] in KNOWN_REFUSALS:
            return error.args[0]
        return "fixture-refused-unknown"
    if isinstance(error, TimeoutError):
        return "fixture-timeout"
    if isinstance(error, FileNotFoundError):
        return "fixture-os-missing"
    if isinstance(error, PermissionError):
        return "fixture-os-denied"
    if isinstance(error, OSError):
        return "fixture-os-other"
    return "fixture-value-invalid"


# Only exact task-owned module names and positive source line numbers may leave
# a refusal. The walk/output are bounded; frame text and locals stay local.
def refusal_frames(error):
    names = frozenset({"ssh_agent_keeper.py", "ssh_agent_loader.py",
                       "ssh_agent_sources.py", "ssh_agent_route.py"})
    directory = os.path.dirname(os.path.abspath(__file__))
    frames = []
    frame = error.__traceback__
    for _ in range(32):
        if frame is None:
            break
        filename = os.path.abspath(frame.tb_frame.f_code.co_filename)
        name = os.path.basename(filename)
        if name in names and filename == os.path.join(directory, name) and 0 < frame.tb_lineno <= 1000000:
            frames.append(name + ":" + str(frame.tb_lineno))
        frame = frame.tb_next
    return ",".join(frames) or "none"


def password_outcome(action, exit_code, output, loaded, remote, failed, connections):
    if len(output) > 8192:
        raise Refused("native password stdout bound")
    lines = output.splitlines()
    markers = ("KEEPER_SELECTED", "KEEPER_AUTHENTICATED", "KEEPER_FAILED",
               "KEEPER_AUTH_ENDED", "KEEPER_REFUSED password", "KEEPER_REFUSED cancelled", "KEEPER_REFUSED expired")
    outcomes = ("success", "unsupported", "invalid_binding", "invalid_request",
                "key_unavailable", "agent_refused", "expired", "revoked", "unavailable", "unknown")
    return {"action": action if action in ("accept", "wrong", "decline", "cancel", "deadline", "transport") else "unknown",
            "exit_code": exit_code if type(exit_code) is int and -255 <= exit_code <= 255 else None,
            "key_prompt_count": min(128, lines.count(b"KEEPER_PROMPT")),
            "remote_prompt_count": min(128, lines.count(b"KEEPER_REMOTE_PROMPT")),
            "loaded": loaded is True, "remote": remote is True, "gateway_failed": failed is True,
            "gateway_connections": min(129, max(0, connections)) if type(connections) is int else None,
            "markers": [marker for marker in markers if marker.encode("ascii") in lines],
            "outcomes": [name for name in outcomes if ("KEEPER_OUTCOME " + name).encode("ascii") in lines],
            "winning_branch": next((name for name in ("authenticate", "events")
                                    if ("KEEPER_PASSWORD_BRANCH " + name).encode("ascii") in lines), "unknown"),
            "answer_enqueued": b"KEEPER_PASSWORD_ANSWER_ENQUEUED true" in lines,
            "offline_seen": b"KEEPER_PASSWORD_OFFLINE_SEEN true" in lines}


def password_receipt(value, expected):
    # Diagnostic-only closed receipt. Generic control failure cannot satisfy it.
    fields = {"type", "outcome", "settled", "offline", "prompts_retired", "agents_removed", "masters"}
    if (type(value) is not dict or set(value) != fields
            or value["type"] != "password_outcome"
            or expected not in ("rejected", "declined", "inconclusive")
            or value["outcome"] != expected
            or any(value[key] is not True for key in ("settled", "offline", "prompts_retired", "agents_removed"))
            or type(value["masters"]) is not int or value["masters"] != 0):
        raise Refused("original password settlement receipt")


VM = "chimaera-isolation-20261002"
LINUX_BINARY = "/fixtures/keeper-ssh-fixture"
LINUX_WRAPPER = "/fixtures/test-keeper-ssh-route-linux.py"
# Closed localhost byte transport only. Buffers are hard bounded before reads;
# neither socket destinations nor executable/source come from the test wire.
BRIDGE = r'''
import os,select,socket,sys,time
p=int(sys.argv[1]); s=socket.create_connection(('127.0.0.1',p),2)
s.setblocking(False); os.set_blocking(0,False); os.set_blocking(1,False)
a=bytearray();b=bytearray();n=0;end=time.monotonic()+25
while time.monotonic()<end:
 r=[0] if len(a)<65536 else []
 if len(b)<65536:r.append(s)
 w=([s] if a else [])+([1] if b else [])
 rr,ww,_=select.select(r,w,[],.05)
 if 0 in rr:
  v=os.read(0,min(8192,65536-len(a)))
  if not v:break
  a.extend(v);n+=len(v)
 if s in rr:
  v=s.recv(min(8192,65536-len(b)))
  if not v:
   while b and time.monotonic()<end:
    if select.select([],[1],[],.05)[1]:del b[:os.write(1,b)]
   break
  b.extend(v);n+=len(v)
 if n>8388608:raise SystemExit(2)
 if s in ww:del a[:s.send(a)]
 if 1 in ww:del b[:os.write(1,b)]
s.close()
'''


def string(packet, offset):
    if offset + 4 > len(packet):
        raise Refused("binding length")
    count = struct.unpack("!I", packet[offset:offset + 4])[0]
    offset += 4
    if count > 65536 or offset + count > len(packet):
        raise Refused("binding body")
    return packet[offset:offset + count], offset + count


class CountingProxy(Proxy):
    def __init__(self, owner, path, upstream, process, action, host_keys):
        self.action, self.host_keys = action, host_keys
        self.legs = [[0, 0], [0, 0]]
        self.entered = threading.Event()
        super().__init__(owner, path, upstream, process, action == "refuse")
        self.RequestHandlerClass = CountingHandler
        self.deadline = owner.end(24)

    def leg_counts(self):
        with self.lock:
            return tuple(tuple(v) for v in self.legs)


class CountingHandler(socketserver.BaseRequestHandler):
    def handle(self):
        owner = self.server
        self.request.settimeout(1)
        if owner.process.poll() is not None or owner.identity(owner.upstream) != owner.upstream_identity:
            raise Refused("external agent identity changed")
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as upstream:
            upstream.settimeout(1)
            upstream.connect(str(owner.upstream))
            leg = None
            while not owner.stopped.is_set() and time.monotonic() < owner.deadline:
                if not select.select([self.request], [], [], .025)[0]:
                    continue
                if not self.request.recv(1, socket.MSG_PEEK):
                    return
                packet = agent_frame(self.request)
                if packet[0] not in (11, 13, 27):
                    raise Refused("external request type")
                with owner.lock:
                    owner.frames += 1
                    if owner.frames > 64:
                        raise Refused("external frame budget")
                    if packet[0] == 27:
                        name, offset = string(packet, 1)
                        host, _ = string(packet, offset)
                        if name != b"session-bind@openssh.com" or host not in owner.host_keys:
                            raise Refused("external binding identity")
                        leg = owner.host_keys.index(host)
                        owner.legs[leg][0] += 1
                    if packet[0] == 13:
                        if leg is None:
                            raise Refused("unbound external signature")
                        owner.legs[leg][1] += 1
                        owner.entered.set()
                if packet[0] == 13 and owner.action in ("cancel", "deadline"):
                    while not owner.stopped.wait(.025) and time.monotonic() < owner.deadline:
                        # The actual caller closing proves refusal settled. No
                        # fabricated response/signature is sent from this hold.
                        if select.select([self.request], [], [], 0)[0] and not self.request.recv(1, socket.MSG_PEEK):
                            return
                    return
                if packet[0] == 13 and owner.refuse:
                    reply = b"\x05"
                else:
                    upstream.sendall(struct.pack("!I", len(packet)) + packet)
                    reply = agent_frame(upstream)
                self.request.sendall(struct.pack("!I", len(reply)) + reply)


class Gateway(socketserver.ThreadingMixIn, socketserver.TCPServer):
    daemon_threads = False
    block_on_close = True

    def __init__(self, owner):
        self.thread = None
        self.started = self.closed = False
        owner.gateways.append(self)  # Partial owner before bind/thread effects.
        self.owner = owner
        self.port = None
        self.stopped = threading.Event()
        self.lock = threading.Lock()
        self.active = set()
        self.slots = threading.BoundedSemaphore(8)
        self.total = 0
        self.failed = False
        super().__init__(("127.0.0.1", 0), GatewayHandler)
        self.origin = "http://127.0.0.1:" + str(self.server_address[1])
        self.thread = threading.Thread(target=self.serve_forever, kwargs={"poll_interval": .025})

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
            self.failed = True

    def close(self):
        if self.closed:
            return
        self.stopped.set()
        with self.lock:
            for connection in list(self.active):
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
                raise Refused("gateway thread cleanup")
        self.closed = True


class GatewayHandler(socketserver.BaseRequestHandler):
    def handle(self):
        gateway = self.server
        with gateway.lock:
            gateway.total += 1
            if gateway.total > 128:
                raise Refused("gateway connection bound")
        if type(gateway.port) is not int or not 1 <= gateway.port <= 65535:
            raise Refused("gateway target receipt")
        process = gateway.owner.management(
            "/usr/bin/python3 -I -S -c " + shlex.quote(BRIDGE) + " " + str(gateway.port),
            input_pipe=True,
        )
        a, b = bytearray(), bytearray()
        end = gateway.owner.end(25)
        count = 0
        try:
            self.request.setblocking(False)
            os.set_blocking(process.stdin.fileno(), False)
            os.set_blocking(process.stdout.fileno(), False)
            while not gateway.stopped.is_set() and time.monotonic() < end:
                read = ([self.request] if len(a) < 65536 else []) + ([process.stdout] if len(b) < 65536 else [])
                write = ([process.stdin] if a else []) + ([self.request] if b else [])
                readable, writable, _ = select.select(read, write, [], .025)
                if self.request in readable:
                    value = self.request.recv(min(8192, 65536 - len(a)))
                    if not value:
                        break
                    a.extend(value)
                    count += len(value)
                if process.stdout in readable:
                    value = os.read(process.stdout.fileno(), min(8192, 65536 - len(b)))
                    if not value:
                        while b and time.monotonic() < end and not gateway.stopped.is_set():
                            if select.select([], [self.request], [], .025)[1]:
                                del b[:self.request.send(b)]
                        break
                    b.extend(value)
                    count += len(value)
                if count > 8388608:
                    raise Refused("gateway byte bound")
                if process.stdin in writable:
                    del a[:os.write(process.stdin.fileno(), a)]
                if self.request in writable:
                    del b[:self.request.send(b)]
        finally:
            if not process.stdin.closed:
                process.stdin.close()
            stop_owned(process)


class Keeper(Sources):
    def __init__(self, binary, podman, digest, wrapper_digest):
        self.gateways = []
        self.controls = []
        self.cleanup_progress = {}
        self.close_deadline = None
        self.podman, self.digest, self.wrapper_digest = podman, digest, wrapper_digest
        self.process_lock = threading.Lock()
        super().__init__(binary)
        self.deadline = time.monotonic() + 120

    def spawn(self, argv, env=None, output=True, input_pipe=False):
        # Register every newly owned host process before a delivered fixture
        # signal can enter the outer cleanup, including native/helper probes.
        signals = {signal.SIGALRM, signal.SIGTERM, signal.SIGINT, signal.SIGHUP}
        previous = signal.pthread_sigmask(signal.SIG_BLOCK, signals)
        try:
            return super().spawn(argv, env, output, input_pipe)
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, previous)

    def management(self, command, input_pipe=False, control=False):
        # Only fixed commands emitted by this runner use the management channel.
        with self.process_lock:
            signals = {signal.SIGALRM, signal.SIGTERM, signal.SIGINT, signal.SIGHUP}
            previous = signal.pthread_sigmask(signal.SIG_BLOCK, signals)
            try:
                process = self.spawn([str(self.podman), "machine", "ssh", "--username", "root", VM, command],
                                     dict(self.env, HOME=os.path.expanduser("~")), input_pipe=input_pipe)
                if control:
                    self.controls.append(process)
                return process
            finally:
                signal.pthread_sigmask(signal.SIG_SETMASK, previous)

    def record(self, process, seconds=5, deadline=None):
        end = self.end(seconds) if deadline is None else min(self.deadline, deadline)
        data = bytearray()
        while time.monotonic() < end:
            if not select.select([process.stdout], [], [], .025)[0]:
                continue
            byte = os.read(process.stdout.fileno(), 1)
            if not byte or len(data) > 16384:
                raise Refused("keeper metadata bound or EOF")
            if byte == b"\n":
                value = json.loads(data)
                if not isinstance(value, dict):
                    raise Refused("keeper metadata object")
                return value
            data.extend(byte)
        raise Refused("keeper metadata deadline")

    @staticmethod
    def closed_receipt(value, expected_directories=None):
        if (not isinstance(value, dict)
                or set(value) != {"type", "masters_absent", "agent_paths_absent", "agent_directories"}
                or value["type"] != "closed" or value["masters_absent"] is not True
                or value["agent_paths_absent"] is not True
                or type(value["agent_directories"]) is not int
                or not 0 <= value["agent_directories"] <= 128
                or (expected_directories is not None and value["agent_directories"] != expected_directories)):
            raise Refused("keeper cleanup receipt")

    def cleanup_state(self, process):
        state = self.cleanup_progress.get(process)
        if state is None:
            if len(self.cleanup_progress) >= 8:
                raise Refused("keeper fixture cleanup uncertain")
            state = {"deadline": self.end(30), "buffer": bytearray(), "inner": None,
                     "external": False, "waited": False, "failed": False}
            self.cleanup_progress[process] = state
        return state

    @staticmethod
    def cleanup_record(process, state):
        # Partial bytes and the original deadline survive interrupted drains.
        # Malformed/EOF data is permanent refusal, never another receipt attempt.
        while time.monotonic() < state["deadline"]:
            if not select.select([process.stdout], [], [], .025)[0]:
                continue
            byte = os.read(process.stdout.fileno(), 1)
            if not byte or len(state["buffer"]) > 16384:
                state["failed"] = True
                raise Refused("keeper fixture cleanup uncertain")
            if byte == b"\n":
                try:
                    value = json.loads(state["buffer"])
                    if type(value) is not dict:
                        raise ValueError()
                except (ValueError, UnicodeError):
                    state["failed"] = True
                    raise Refused("keeper fixture cleanup uncertain") from None
                state["buffer"].clear()
                return value
            state["buffer"].extend(byte)
        state["failed"] = True
        raise Refused("keeper fixture cleanup uncertain")

    def cleanup_receipts(self, process, expected_directories=None):
        state = self.cleanup_state(process)
        if state["failed"]:
            raise Refused("keeper fixture cleanup uncertain")
        if state["inner"] is None:
            value = self.cleanup_record(process, state)
            try:
                self.closed_receipt(value, expected_directories)
            except Refused:
                state["failed"] = True
                raise
            state["inner"] = value
        else:
            try:
                self.closed_receipt(state["inner"], expected_directories)
            except Refused:
                state["failed"] = True
                raise
        if not state["external"]:
            value = self.cleanup_record(process, state)
            if (set(value) != {"type", "cgroup_empty", "cgroup_removed"}
                    or value["type"] != "external_cleanup"
                    or value["cgroup_empty"] is not True or value["cgroup_removed"] is not True):
                state["failed"] = True
                raise Refused("keeper fixture cleanup uncertain")
            state["external"] = True
        if not state["waited"]:
            remaining = state["deadline"] - time.monotonic()
            if remaining <= 0:
                state["failed"] = True
                raise Refused("keeper fixture cleanup uncertain")
            process.wait(timeout=min(2, remaining))
            if process.returncode:
                state["failed"] = True
                raise Refused("keeper fixture cleanup uncertain")
            state["waited"] = True

    def cleanup_control(self, process, expected_directories=None):
        state = None
        completed = False
        try:
            state = self.cleanup_state(process)
            # A closed writer already delivered EOF. An exited child still owes
            # both receipts; return code zero alone cannot discharge this owner.
            if process.poll() is None and not process.stdin.closed:
                try:
                    if os.write(process.stdin.fileno(), b"EXIT\n") != 5:
                        raise Refused("keeper fixture cleanup uncertain")
                except BrokenPipeError:
                    pass  # A genuine cleanup receipt must still prove closure.
            if not process.stdin.closed:
                process.stdin.close()
            self.cleanup_receipts(process, expected_directories)
            completed = True
        finally:
            if not completed and state is not None:
                state["failed"] = True
            stop_owned(process)

    @staticmethod
    def cleanup_failure(failed, original_failure):
        if failed:
            print("REFUSED keeper cleanup: uncertain", file=sys.stderr)
            if not original_failure:
                raise Refused("keeper fixture cleanup uncertain")

    def cleanup_case(self, app, gateway, control, proxy=None):
        original_failure = sys.exc_info()[0] is not None
        failed = False
        actions = [lambda: app.stdin.close() if not app.stdin.closed else None,
                   lambda: stop_owned(app)]
        if proxy is not None:
            actions.append(proxy.close)
        actions.extend([gateway.close, lambda: self.cleanup_control(control, expected_directories=2)])
        for action in actions:
            try:
                action()
            except Exception:
                failed = True
        self.cleanup_failure(failed, original_failure)

    def close(self):
        original_failure = sys.exc_info()[0] is not None
        if self.close_deadline is None:
            self.close_deadline = time.monotonic() + 30
        self.deadline = self.close_deadline
        failed = False
        # Native/app observers die first; then bridges, then original keeper.
        for process in reversed(self.owned):
            if process not in self.controls:
                try:
                    stop_owned(process)
                except Exception:
                    failed = True
        for gateway in reversed(self.gateways):
            try:
                gateway.close()
            except Exception:
                failed = True
        for process in reversed(self.controls):
            try:
                self.cleanup_control(process)
            except Exception:
                failed = True
        try:
            super().close()
        except Exception:
            failed = True
        self.cleanup_failure(failed, original_failure)

    def run(self):
        from ssh_agent_loader import capture
        for path, digest in ((LINUX_BINARY, self.digest), (LINUX_WRAPPER, self.wrapper_digest)):
            check = self.management("/usr/bin/sha256sum " + path)
            try:
                output = capture(check, self.end(5), 256)
                if check.returncode:
                    raise Refused("Linux fixture management check failed")
                if output.split() != [digest.encode(), path.encode()]:
                    raise Refused("Linux fixture immutable digest")
            finally:
                stop_owned(check)
        encrypted, public = self.key("selected")
        duplicate = self.root / "external-duplicate"
        duplicate.write_bytes(encrypted.read_bytes())
        os.chmod(duplicate, 0o600)
        if self.collect(["/usr/bin/ssh-keygen", "-q", "-p", "-P", "fixture-only-passphrase", "-N", "", "-f", str(duplicate)])[0]:
            raise Refused("synthetic duplicate preparation")
        upstream = self.root / "external-agent"
        agent = self.spawn(["/usr/bin/ssh-agent", "-D", "-P", "", "-a", str(upstream)], output=False)
        until = self.end(2)
        while not upstream.exists() and time.monotonic() < until:
            time.sleep(.02)
        external_env = dict(self.env, SSH_AUTH_SOCK=str(upstream), SSH_ASKPASS_REQUIRE="never")
        if self.collect(["/usr/bin/ssh-add", str(duplicate)], external_env)[0]:
            raise Refused("external seed")
        before = self.identities(external_env)
        foreign, _ = self.key("foreign", passphrase="")
        foreign_path = self.root / "foreign-agent"
        self.spawn(["/usr/bin/ssh-agent", "-D", "-P", "", "-a", str(foreign_path)], output=False)
        until = self.end(2)
        while not foreign_path.exists() and time.monotonic() < until:
            time.sleep(.02)
        foreign_env = dict(self.env, SSH_AUTH_SOCK=str(foreign_path), SSH_ASKPASS_REQUIRE="never")
        if self.collect(["/usr/bin/ssh-add", str(foreign)], foreign_env)[0]:
            raise Refused("foreign seed")
        foreign_before = self.identities(foreign_env)
        for action in ("accept", "refuse", "cancel", "deadline"):
            gateway = Gateway(self)
            control = self.management("/usr/bin/env -i PATH=/usr/bin:/bin:/usr/sbin HOME=/tmp /usr/bin/python3 -I -S " + LINUX_WRAPPER, input_pipe=True, control=True)
            body = json.dumps({"advertised_port": gateway.server_address[1],
                               "public_key": "ssh-ed25519 " + public.decode()}, separators=(",", ":")).encode() + b"\n"
            if os.write(control.stdin.fileno(), body) != len(body):
                raise Refused("keeper bootstrap handoff")
            ready = self.record(control, 20)
            if (ready.get("type") != "ready" or type(ready.get("pid")) is not int or ready["pid"] != ready.get("pgid")
                    or not 1 <= ready.get("port", 0) <= 65535 or len(ready.get("ssh_ports", [])) != 2
                    or len(ready.get("host_keys", [])) != 2):
                raise Refused("keeper startup identity")
            gateway.port = ready["port"]
            host_keys = []
            known = self.root / ("known-" + action)
            rows = []
            for port, host in zip(ready["ssh_ports"], ready["host_keys"]):
                parts = host.split()
                if type(port) is not int or not 1 <= port <= 65535 or len(parts) != 3 or parts[0] != "ssh-ed25519":
                    raise Refused("sshd public receipt")
                host_keys.append(base64.b64decode(parts[1], validate=True))
                rows.append("[127.0.0.1]:" + str(port) + " " + " ".join(parts[:2]) + "\n")
            known.write_text("".join(rows))
            os.chmod(known, 0o600)
            proxy = CountingProxy(self, self.root / ("proxy-" + action), upstream, agent, action, host_keys)
            proxy.start()
            gateway.start()
            config = self.root / ("config-" + action)
            config.write_text("Host keeper-route-fixture\n HostName 127.0.0.1\n Port " + str(ready["ssh_ports"][1]) +
                "\n ProxyJump keeper-route-hop\n IdentityAgent " + str(proxy.path) + "\n IdentitiesOnly no\n"
                "Host keeper-route-hop\n HostName 127.0.0.1\n Port " + str(ready["ssh_ports"][0]) + "\n IdentityAgent none\n IdentitiesOnly yes\n"
                "Host *\n User root\n IdentityFile " + str(encrypted) + "\n UserKnownHostsFile " + str(known) +
                "\n GlobalKnownHostsFile none\n PubkeyAuthentication yes\n PasswordAuthentication no\n KbdInteractiveAuthentication no\n"
                " GSSAPIAuthentication no\n HostbasedAuthentication no\n PreferredAuthentications publickey\n"
                " HostKeyAlgorithms ssh-ed25519\n PubkeyAcceptedAlgorithms ssh-ed25519\n CASignatureAlgorithms ssh-ed25519\n ControlMaster no\n ControlPath none\n")
            os.chmod(config, 0o600)
            app = self.spawn([str(self.binary), "--keeper-route-fixture", str(config), gateway.origin, action], self.env, input_pipe=True)
            receipts, answered, output = [], 0, bytearray()
            cancel_sent = False
            end = self.end(23)
            try:
                while True:
                    if time.monotonic() >= end:
                        raise Refused("native keeper case deadline")
                    if action == "cancel" and proxy.entered.is_set() and not cancel_sent:
                        if os.write(app.stdin.fileno(), b"CANCEL\n") != 7:
                            raise Refused("original cancellation handoff")
                        app.stdin.close()
                        cancel_sent = True
                    if not select.select([app.stdout], [], [], .025)[0]:
                        continue
                    chunk = os.read(app.stdout.fileno(), 4096)
                    if not chunk:
                        break
                    if len(output) + len(chunk) > 8192:
                        raise Refused("native keeper stdout bound")
                    output.extend(chunk)
                    count = output.count(b"KEEPER_PROMPT\n")
                    if count > 2:
                        raise Refused("native keeper prompt count")
                    while answered < count:
                        receipts.append(self.helper_receipt(app))
                        if os.write(app.stdin.fileno(), b"CONTINUE\n") != 9:
                            raise Refused("native keeper passphrase handoff")
                        answered += 1
                        if answered == 2 and action != "cancel":
                            app.stdin.close()
                app.wait(timeout=max(.01, end - time.monotonic()))
                expected = {"accept": b"KEEPER_AUTHENTICATED\n", "refuse": b"KEEPER_REFUSED external\n",
                            "cancel": b"KEEPER_REFUSED cancelled\n", "deadline": b"KEEPER_REFUSED expired\n"}[action]
                counts = proxy.leg_counts()
                if app.returncode or expected not in output or answered != 2 or counts != ((0, 0), (1, 1)) or proxy.failed or gateway.failed:
                    # Fixed synthetic counters/markers only, never child bytes
                    # or credential-bearing protocol/error text.
                    print("DIAGNOSTIC keeper", json.dumps({
                        "action": action, "exit_code": app.returncode,
                        "prompts": answered, "selected": b"KEEPER_SELECTED\n" in output,
                        "authenticated": b"KEEPER_AUTHENTICATED\n" in output,
                        "failed_marker": b"KEEPER_FAILED\n" in output,
                        "expected_marker": expected in output, "leg_counts": counts,
                        "outcomes": [name for name in ("success", "unsupported", "invalid_binding",
                            "invalid_request", "key_unavailable", "agent_refused", "expired",
                            "revoked", "unavailable", "unknown")
                            if ("KEEPER_OUTCOME " + name + "\n").encode("ascii") in output],
                        "proxy_failed": proxy.failed, "gateway_failed": gateway.failed,
                        "gateway_connections": gateway.total,
                    }, separators=(",", ":")), flush=True)
                    raise Refused("actual keeper per-leg source/refusal")
                for receipt in receipts:
                    self.cleanup_receipt(receipt)
                if action == "accept":
                    # The native process and short signing authority have ended.
                    # CHECK performs only three immediate existing-master reads.
                    os.write(control.stdin.fileno(), b"CHECK\n")
                    retained = self.record(control, 12)
                    if retained.get("type") != "retained" or retained.get("reads") != 3 or retained.get("jobs") != 0 or retained.get("masters", 0) < 1:
                        raise Refused("short retained-master receipt")
                    os.write(control.stdin.fileno(), b"RETIRE\n")
                    retired = self.record(control, 8)
                    if retired != {"type": "retired", "masters": retained["masters"]} or proxy.leg_counts() != counts:
                        raise Refused("retained master retired without new signing")
                    os.write(control.stdin.fileno(), b"REVOKE\n")
                    if self.record(control, 8) != {"type": "revoked", "new_auth_refused": True} or proxy.leg_counts() != counts:
                        raise Refused("revocation cannot start new authentication")
                if self.identities(external_env) != before or self.identities(foreign_env) != foreign_before:
                    raise Refused("synthetic agents changed")
                print("PASS", action, "private_external_signs0 target_external_signs1 original_owner_cleanup", flush=True)
            finally:
                self.cleanup_case(app, gateway, control, proxy)
        print("PASS keeper4: actual router/grants/two sshd key legs; synthetic account/prompts/Slurm, no job lifetime", flush=True)


class PasswordKeeper(Keeper):
    """B: actual key bastion/password target; root-password, PAM-free only."""
    def run(self):
        from ssh_agent_loader import capture
        for path, digest in ((LINUX_BINARY, self.digest), (LINUX_WRAPPER, self.wrapper_digest)):
            check = self.management("/usr/bin/sha256sum " + path)
            try:
                if capture(check, self.end(5), 256).split() != [digest.encode(), path.encode()] or check.returncode:
                    raise Refused("Linux fixture immutable digest")
            finally:
                stop_owned(check)
        encrypted, public = self.key("password-bastion")
        foreign, _ = self.key("password-foreign", passphrase="")
        foreign_path = self.root / "password-foreign-agent"
        self.spawn(["/usr/bin/ssh-agent", "-D", "-P", "", "-a", str(foreign_path)], output=False)
        until = self.end(2)
        while not foreign_path.exists() and time.monotonic() < until:
            time.sleep(.02)
        foreign_env = dict(self.env, SSH_AUTH_SOCK=str(foreign_path), SSH_ASKPASS_REQUIRE="never")
        if self.collect(["/usr/bin/ssh-add", str(foreign)], foreign_env)[0]:
            raise Refused("foreign seed")
        foreign_before = self.identities(foreign_env)
        commands = {"accept": b"GOOD\n", "wrong": b"BAD\n", "decline": b"DECLINE\n",
                    "cancel": b"CANCEL\n", "deadline": b"HOLD\n", "transport": b"BAD\n"}
        for action, answer in commands.items():
            gateway = Gateway(self)
            control = self.management("/usr/bin/env -i PATH=/usr/bin:/bin:/usr/sbin HOME=/tmp /usr/bin/python3 -I -S " + LINUX_WRAPPER,
                                      input_pipe=True, control=True)
            body = json.dumps({"advertised_port": gateway.server_address[1], "mixed_password": True,
                               "public_key": "ssh-ed25519 " + public.decode()}, separators=(",", ":")).encode() + b"\n"
            # Begin conservatively before the Linux ready/arm emission. This
            # receipt clock never extends native's original fifteen seconds.
            proof_deadline = self.end(15)
            if os.write(control.stdin.fileno(), body) != len(body):
                raise Refused("keeper bootstrap handoff")
            ready = self.record(control, 20)
            if (ready.get("type") != "ready" or ready.get("mixed_password") is not True
                    or type(ready.get("pid")) is not int or ready["pid"] != ready.get("pgid")
                    or type(ready.get("port")) is not int or not 1 <= ready["port"] <= 65535
                    or len(ready.get("ssh_ports", [])) != 2 or len(ready.get("host_keys", [])) != 2):
                raise Refused("password topology receipt")
            gateway.port = ready["port"]
            known = self.root / ("password-known-" + action)
            rows = []
            for port, host in zip(ready["ssh_ports"], ready["host_keys"]):
                parts = host.split()
                if type(port) is not int or not 1 <= port <= 65535 or len(parts) != 3 or parts[0] != "ssh-ed25519":
                    raise Refused("sshd public receipt")
                if len(base64.b64decode(parts[1], validate=True)) > 8192:
                    raise Refused("sshd public bound")
                rows.append("[127.0.0.1]:" + str(port) + " " + " ".join(parts[:2]) + "\n")
            known.write_text("".join(rows))
            os.chmod(known, 0o600)
            config = self.root / ("password-config-" + action)
            config.write_text("Host keeper-route-fixture\n HostName 127.0.0.1\n Port " + str(ready["ssh_ports"][1]) +
                "\n ProxyJump keeper-route-hop\n IdentityFile none\n IdentityAgent none\n PubkeyAuthentication no\n"
                " PasswordAuthentication yes\n KbdInteractiveAuthentication no\n PreferredAuthentications password\n"
                "Host keeper-route-hop\n HostName 127.0.0.1\n Port " + str(ready["ssh_ports"][0]) +
                "\n IdentityFile " + str(encrypted) + "\n IdentityAgent none\n IdentitiesOnly yes\n"
                " PubkeyAuthentication yes\n PasswordAuthentication no\n KbdInteractiveAuthentication no\n PreferredAuthentications publickey\n"
                "Host *\n User root\n UserKnownHostsFile " + str(known) + "\n GlobalKnownHostsFile none\n"
                " GSSAPIAuthentication no\n HostbasedAuthentication no\n HostKeyAlgorithms ssh-ed25519\n"
                " PubkeyAcceptedAlgorithms ssh-ed25519\n CASignatureAlgorithms ssh-ed25519\n ControlMaster no\n ControlPath none\n")
            os.chmod(config, 0o600)
            gateway.start()
            app = self.spawn([str(self.binary), "--keeper-route-fixture", str(config), gateway.origin,
                              "password-" + ("wrong" if action == "transport" else action)], self.env, input_pipe=True)
            output, receipts, loaded, remote = bytearray(), [], False, False
            end = self.end(23)
            try:
                while True:
                    if time.monotonic() >= end:
                        raise Refused("native password case deadline")
                    if not select.select([app.stdout], [], [], .025)[0]:
                        continue
                    chunk = os.read(app.stdout.fileno(), 4096)
                    if not chunk:
                        break
                    if len(output) + len(chunk) > 8192:
                        raise Refused("native password stdout bound")
                    output.extend(chunk)
                    if output.count(b"KEEPER_PROMPT\n") > 1 or output.count(b"KEEPER_REMOTE_PROMPT\n") > 1:
                        raise Refused("native password prompt count")
                    if b"KEEPER_PROMPT\n" in output and not loaded:
                        receipts.append(self.helper_receipt(app))
                        if os.write(app.stdin.fileno(), b"CONTINUE\n") != 9:
                            raise Refused("private key load handoff")
                        loaded = True
                    if b"KEEPER_REMOTE_PROMPT\n" in output and not remote:
                        if not loaded:
                            raise Refused("original routed prompt handoff")
                        if action == "transport":
                            # Close the actual grant/HTTP bridge without an
                            # answer. Client failure cannot prove auth denial.
                            gateway.close()
                        elif os.write(app.stdin.fileno(), answer) != len(answer):
                            raise Refused("original routed prompt handoff")
                        app.stdin.close()
                        remote = True
                app.wait(timeout=max(.01, end - time.monotonic()))
                expected = {"accept": b"KEEPER_AUTHENTICATED\n", "wrong": b"KEEPER_AUTH_ENDED\n",
                            "decline": b"KEEPER_AUTH_ENDED\n", "cancel": b"KEEPER_REFUSED cancelled\n",
                            "deadline": b"KEEPER_REFUSED expired\n", "transport": b"KEEPER_AUTH_ENDED\n"}[action]
                if app.returncode or expected not in output or not loaded or not remote or gateway.failed:
                    # Project only fixed markers/counters; never child text or credentials.
                    with gateway.lock:
                        connections, failed = gateway.total, gateway.failed
                    diagnostic = password_outcome(action, app.returncode, output, loaded, remote, failed, connections)
                    print("DIAGNOSTIC keeper_password", json.dumps(diagnostic, separators=(",", ":")), flush=True)
                    raise Refused("actual password route outcome")
                if action in ("wrong", "decline", "transport"):
                    if time.monotonic() >= proof_deadline:
                        raise Refused("original password settlement deadline")
                    if os.write(control.stdin.fileno(), b"PASSWORD_OUTCOME\n") != 17:
                        raise Refused("original password settlement handoff")
                    receipt = self.record(control, deadline=proof_deadline)
                    password_receipt(receipt, {"wrong": "rejected", "decline": "declined", "transport": "inconclusive"}[action])
                    if action == "transport" and (b"KEEPER_AUTHENTICATED\n" in output or b"KEEPER_REFUSED password\n" in output):
                        raise Refused("transport cannot prove password rejection")
                for receipt in receipts:
                    self.cleanup_receipt(receipt)
                if action == "accept":
                    os.write(control.stdin.fileno(), b"CHECK\n")
                    retained = self.record(control, 12)
                    if retained.get("type") != "retained" or retained.get("reads") != 3 or retained.get("jobs") != 0 or retained.get("masters", 0) < 1:
                        raise Refused("password retained-master receipt")
                    os.write(control.stdin.fileno(), b"RETIRE\n")
                    if self.record(control, 8) != {"type": "retired", "masters": retained["masters"]}:
                        raise Refused("password master retirement")
                    os.write(control.stdin.fileno(), b"REVOKE\n")
                    if self.record(control, 8) != {"type": "revoked", "new_auth_refused": True}:
                        raise Refused("password owner revocation")
                if self.identities(foreign_env) != foreign_before:
                    raise Refused("foreign agent changed")
                print("PASS password_" + action + " original_leg_prompt authenticated_master_or_refusal cleanup", flush=True)
            finally:
                self.cleanup_case(app, gateway, control)
        print("PASS keeper_password6: actual key/password route + transport-loss negative; original server/decline settlement; synthetic account/UI/root shadow, UsePAM=no; no MFA/job proof", flush=True)


def cleanup_self_test():
    """Local controlled pipes/processes only; no keys, agents or VM access."""
    import contextlib
    import io
    import subprocess
    from types import SimpleNamespace

    inner = json.dumps({"type": "closed", "masters_absent": True,
                        "agent_paths_absent": True, "agent_directories": 2}).encode() + b"\n"
    outer = json.dumps({"type": "external_cleanup", "cgroup_empty": True,
                        "cgroup_removed": True}).encode() + b"\n"
    children = []

    def owner():
        value = Keeper.__new__(Keeper)
        value.cleanup_progress = {}
        value.deadline = time.monotonic() + 3
        return value

    def child(code):
        process = subprocess.Popen([sys.executable, "-I", "-S", "-c", code],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.DEVNULL, env={"PATH": "/usr/bin:/bin"},
                                   start_new_session=True)
        children.append(process)
        return process

    def refuse(action):
        try:
            action()
        except Refused:
            return
        raise AssertionError("cleanup unexpectedly accepted")

    try:
        value = owner()
        process = child("import os; os.read(0,1); os.write(1," + repr(inner + outer) + ")")
        process.stdin.close()
        value.cleanup_control(process, 2)
        state = value.cleanup_progress[process]
        deadline = state["deadline"]
        assert state["inner"] is not None and state["external"] and state["waited"]
        value.cleanup_control(process, 2)
        assert state["deadline"] == deadline and process.stdout.closed

        value = owner()
        process = child("import os; os.write(1," + repr(inner + outer) + ")")
        process.wait(timeout=2)  # Exit before any drain must still owe receipts.
        value.cleanup_control(process, 2)
        assert value.cleanup_progress[process]["waited"]

        value = owner()
        process = child("import os; os.write(1," + repr(inner + outer[:12]) +
                        "); os.read(0,1); os.write(1," + repr(outer[12:]) + ")")
        state = value.cleanup_state(process)
        deadline = state["deadline"]
        original_select = select.select
        interrupted = False

        def interrupt_select(read, write, error, timeout):
            nonlocal interrupted
            if process.stdout in read and state["inner"] is not None and len(state["buffer"]) == 12 and not interrupted:
                interrupted = True
                raise RuntimeError("synthetic drain interruption")
            return original_select(read, write, error, timeout)

        select.select = interrupt_select
        try:
            try:
                value.cleanup_receipts(process, 2)
            except RuntimeError:
                pass
            else:
                raise AssertionError("missing synthetic interruption")
        finally:
            select.select = original_select
        assert interrupted and state["inner"] is not None and state["buffer"] == outer[:12]
        assert not state["external"] and not state["waited"] and state["deadline"] == deadline
        os.write(process.stdin.fileno(), b"C")
        value.cleanup_control(process, 2)
        assert state["external"] and state["waited"] and state["deadline"] == deadline

        for data in (inner + outer[:12], inner + outer.replace(b"true", b"1"), b""):
            value = owner()
            process = child("import os; os.write(1," + repr(data) + ")")
            refuse(lambda: value.cleanup_control(process, 2))
            state = value.cleanup_progress[process]
            deadline = state["deadline"]
            refuse(lambda: value.cleanup_control(process, 2))
            assert state["failed"] and not state["waited"] and state["deadline"] == deadline
            assert process.returncode is not None and process.stdout.closed

        value = owner()
        control = child("import os; os.write(1," + repr(inner + outer[:12]) + ")")
        app = child("pass")
        gateway = SimpleNamespace(close=lambda: None)
        diagnostic = io.StringIO()
        with contextlib.redirect_stderr(diagnostic):
            try:
                try:
                    raise Refused("synthetic original case failure")
                finally:
                    value.cleanup_case(app, gateway, control)
            except Refused as error:
                assert error.args == ("synthetic original case failure",)
            else:
                raise AssertionError("original failure was lost")
        assert diagnostic.getvalue() == "REFUSED keeper cleanup: uncertain\n"
        assert control.returncode is not None and control.stdout.closed
    finally:
        for process in reversed(children):
            stop_owned(process)
    print("PASS keeper cleanup retained receipt progress", flush=True)


def main():
    if sys.argv[1:] == ["--cleanup-self-test"]:
        cleanup_self_test()
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--podman", type=pathlib.Path, default=pathlib.Path("/opt/homebrew/bin/podman"))
    parser.add_argument("--linux-sha256", required=True)
    parser.add_argument("--wrapper-sha256", required=True)
    parser.add_argument("--scenario", choices=("key", "mixed-password"), default="key")
    args = parser.parse_args()
    args.podman = args.podman.resolve(strict=True)
    if sys.platform != "darwin" or any(len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest)
                                       for digest in (args.linux_sha256, args.wrapper_sha256)):
        raise Refused("Mac fixture/exact Linux binary and wrapper digests required")
    for path in (args.binary, args.podman):
        info = path.lstat()
        if not path.is_absolute() or not stat.S_ISREG(info.st_mode) or info.st_uid not in (0, os.getuid()) or info.st_mode & 0o022:
            raise Refused("fixed binary ownership")
    def interrupted(*_):
        raise Refused("keeper fixture interrupted or expired")
    for name in (signal.SIGALRM, signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(name, interrupted)
    signals = {signal.SIGALRM, signal.SIGTERM, signal.SIGINT, signal.SIGHUP}
    previous = signal.pthread_sigmask(signal.SIG_BLOCK, signals)
    try:
        constructor = Keeper if args.scenario == "key" else PasswordKeeper
        fixture = constructor(args.binary, args.podman, args.linux_sha256, args.wrapper_sha256)
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous)
    signal.setitimer(signal.ITIMER_REAL, 120)
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
    except (Refused, OSError, ValueError, TimeoutError) as error:
        phase = refusal_phase(error)
        print("REFUSED keeper fixture:", phase, file=sys.stderr)
        if phase in ("fixture-refused-unknown", "fixture-value-invalid"):
            print("REFUSED source frames:", refusal_frames(error), file=sys.stderr)
        raise SystemExit(2)
