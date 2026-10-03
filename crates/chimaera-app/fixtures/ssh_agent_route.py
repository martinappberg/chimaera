#!/usr/bin/env python3
"""Mac encrypted configured identity -> real Link grant/Ready lifecycle.

The keeper/account HTTP and WebSocket protocol is simulated on loopback. This
does not prove real keeper SSH signing, Tauri prompts, or retained HPC jobs.
Uses the loader's bounded synthetic process ownership/cleanup helpers.
"""
import argparse
import base64
import errno
import hashlib
import http.server
import json
import os
import pathlib
import select
import signal
import socketserver
import struct
import subprocess
import sys
import threading
import time

from ssh_agent_loader import Fixture, Refused, stop_owned

TOKEN = "synthetic-route-device-token"
BOOT = "synthetic-route-keeper-boot"
GRANT = "synthetic-route-grant"
GRANT_PATH = "/v1/hosts/fixture-host/ssh/auth/route-grants"
DESTINATION = {"hostname": "fixture.example.invalid", "user": "fixture", "port": 22}


def native_phases(output):
    facts = []
    # Ignore incomplete/unknown child text, including a partial last marker.
    for line in output.split(b"\n")[:-1]:
        fields = line.split(b" ")
        if (len(fields) != 4 or fields[0] != b"ROUTE_PHASE"
                or fields[1] not in (b"client_build", b"me", b"cap_response", b"cap_validate")
                or fields[2] not in (b"begin", b"ok", b"request", b"deadline", b"unsupported")
                or not 1 <= len(fields[3]) <= 5 or not fields[3].isdigit()
                or int(fields[3]) > 12000):
            continue
        facts.append({"phase": fields[1].decode("ascii"), "result": fields[2].decode("ascii"),
                      "elapsed_ms": int(fields[3])})
        if len(facts) == 10:
            break
    return facts


def response_fact(method, path, status, elapsed_ms):
    return {"method": method if method in ("GET", "POST", "DELETE") else "other",
            "path": {"/v1/me": "me", "/v1/ssh/auth/capabilities": "capabilities",
                     "/v1/oauth/refresh": "refresh"}.get(path, "other"),
            "status": status if status in (200, 401, 404) else "other",
            "elapsed_ms": max(0, min(12000, elapsed_ms))}


# Never render an exception message, argv, path or child bytes. Refused reasons
# are projected only from closed fixture literals; unknown values stay opaque.
def failure_fact(error):
    fact = {"kind": "other", "errno": "none", "reason": "other"}
    if isinstance(error, Refused):
        fact["kind"] = "refused"
        reasons = {
            "child receipt shape": "receipt-shape",
            "helper directory receipt": "receipt-directory",
            "helper ownership receipt": "receipt-owner",
            "helper descendant receipt": "receipt-members",
            "missing helper receipt": "receipt-missing",
            "process identity shape": "identity-shape",
            "owned process group changed": "group-changed",
            "fixed command deadline": "command-deadline",
            "fixed command output bound": "command-output-bound",
            "route case deadline": "case-deadline",
            "route prompt handoff": "answer-short",
            "route cancellation handoff": "cancel-short",
            "route fixture interrupted or expired": "interrupted",
            "owned helper cleanup not positive": "helper-cleanup",
            "route case refused": "case-refused",
            "route case output bound": "case-output-bound",
            "positive ordered grant receipt": "grant-receipt",
            "config revalidation escaped": "config-revalidation",
            "late grant or reconnect escaped": "late-grant",
            "ready deadline cleanup receipt": "ready-cleanup",
            "original route deadline restarted": "deadline-restarted",
            "foreign fixture agent changed": "foreign-changed",
            "loopback protocol owner failed": "protocol-failed",
            "loopback helper cleanup": "protocol-cleanup",
            "fixture root identity changed": "root-changed",
        }
        if len(error.args) == 1 and type(error.args[0]) is str:
            fact["reason"] = reasons.get(error.args[0], "other")
    elif isinstance(error, (subprocess.TimeoutExpired, TimeoutError)):
        fact["kind"] = "timeout"
    elif isinstance(error, OSError):
        fact["kind"] = "os"
        fact["errno"] = {errno.ENOENT: "missing", errno.ESRCH: "gone",
                         errno.EACCES: "permission", errno.EPERM: "permission",
                         errno.EPIPE: "broken-pipe", errno.EBADF: "bad-fd"}.get(error.errno, "other")
    elif isinstance(error, ValueError):
        fact["kind"] = "value"
    return fact


def diagnostic_print(*fields):
    # A closed diagnostic sink must not replace the causal fixture/cleanup error.
    try:
        print(*fields, flush=True)
    except (OSError, ValueError):
        pass


class CaseFacts:
    PHASES = ("case-start", "app-spawn", "helper-receipt", "receipt-complete",
              "answer-delay", "answer-write", "answer-sent", "answer-close",
              "answer-closed", "cancel-write", "app-wait", "receipt-cleanup",
              "foreign-check", "case-complete")

    def __init__(self, start):
        self.start = start
        self.phase = "case-start"
        self.phases = []
        self.original_error = None
        self.cleanup_errors = []
        self.receipt_captured = self.continue_sent = False
        self.stdin_closed = False
        self.member_count = 0
        self.pre_cleanup_exit = None
        self.stop_owned_entered = self.stop_owned_live_before = self.stop_owned_returned = False
        self.mark("case-start")

    def mark(self, phase):
        if phase not in self.PHASES:
            raise Refused("route diagnostic phase refused")
        self.phase = phase
        if len(self.phases) < 16:
            self.phases.append({"phase": phase,
                                "elapsed_ms": max(0, min(12000, int((time.monotonic() - self.start) * 1000)))})

    def cleanup_error(self, phase, error):
        if phase not in ("stdin-close", "protocol-close", "app-stop", "protocol-failed"):
            raise Refused("route diagnostic cleanup phase refused")
        if len(self.cleanup_errors) < 4:
            self.cleanup_errors.append({"phase": phase, **failure_fact(error)})

    def fact(self):
        return {"phase": self.phase, "phases": self.phases,
                "original_error": self.original_error, "cleanup_errors": self.cleanup_errors,
                "receipt_captured": self.receipt_captured, "continue_sent": self.continue_sent,
                "stdin_closed": self.stdin_closed, "member_count": self.member_count,
                "pre_cleanup_exit": self.pre_cleanup_exit,
                "stop_owned_entered": self.stop_owned_entered,
                "stop_owned_live_before": self.stop_owned_live_before,
                "stop_owned_returned": self.stop_owned_returned}


class Protocol(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = False
    block_on_close = True

    def server_bind(self):
        # HTTPServer resolves its name before construction returns. Ambient
        # reverse DNS must not consume this literal-loopback fixture's deadline.
        socketserver.TCPServer.server_bind(self)
        self.server_name = "127.0.0.1"
        self.server_port = self.server_address[1]

    def __init__(self, owner, action, public, host):
        self.thread = None
        self.started = False
        self.closed = False
        # Retain this partial owner before bind or thread creation can fail.
        owner.protocols.append(self)
        self.action, self.public, self.host = action, public, host
        self.stopped = threading.Event()
        self.grant_entered = threading.Event()
        self.ready_entered = threading.Event()
        self.lock = threading.Lock()
        # reply() can run under self.lock; this independent lock never spans IO.
        self.diagnostic_lock = threading.Lock()
        self.response_headers_started = []
        self.response_overflow = 0
        self.slots = threading.BoundedSemaphore(8)
        self.requests = self.grants = self.ready = self.reconnects = self.deletes = 0
        self.failed = False
        self.failure_kind = "none"
        self.failure_lines = []
        self.created_at = time.monotonic()
        self.deadline = self.created_at + 12
        super().__init__(("127.0.0.1", 0), Handler)
        self.origin = "http://127.0.0.1:" + str(self.server_port)
        self.thread = threading.Thread(target=self.serve_forever, kwargs={"poll_interval": 0.025})

    def response_started(self, method, path, status):
        fact = response_fact(method, path, status, int((time.monotonic() - self.created_at) * 1000))
        with self.diagnostic_lock:
            if len(self.response_headers_started) < 8:
                self.response_headers_started.append(fact)
            else:
                self.response_overflow = min(64, self.response_overflow + 1)

    def start(self):
        # Defer delivered fixture signals only through the tiny start/receipt
        # transition: cleanup must know whether shutdown has a serving owner.
        signals = {signal.SIGALRM, signal.SIGTERM, signal.SIGINT, signal.SIGHUP}
        previous = signal.pthread_sigmask(signal.SIG_BLOCK, signals)
        try:
            self.thread.start()
            self.started = True
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, previous)

    def process_request(self, request, client_address):
        with self.lock:
            self.requests += 1
            refused = self.requests > 64
        if refused or not self.slots.acquire(blocking=False):
            self.failed = True
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()

    def handle_error(self, request, client_address):
        self.failed = True
        error = sys.exc_info()[1]
        self.failure_kind = ("timeout" if isinstance(error, TimeoutError)
                             else "refused" if isinstance(error, Refused)
                             else "os" if isinstance(error, OSError)
                             else "value" if isinstance(error, ValueError) else "other")
        frame = error.__traceback__ if error is not None else None
        lines = []
        for _ in range(32):
            if frame is None:
                break
            if (os.path.abspath(frame.tb_frame.f_code.co_filename) == os.path.abspath(__file__)
                    and 0 < frame.tb_lineno <= 1000000 and len(lines) < 8):
                lines.append(frame.tb_lineno)
            frame = frame.tb_next
        self.failure_lines = lines  # No text, argument, source or local values.

    def close(self):
        if self.closed:
            return
        # Safe even when construction failed before bind or start.
        stopped = getattr(self, "stopped", None)
        if stopped is not None:
            stopped.set()
        if self.started and self.thread.is_alive():
            self.shutdown()
        if hasattr(self, "socket"):
            self.server_close()
        if self.started:
            self.thread.join(timeout=1)
            if self.thread.is_alive():
                raise Refused("loopback helper cleanup")
        self.closed = True


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def setup(self):
        super().setup()
        self.connection.settimeout(1)

    def send_response(self, code, message=None):
        self.server.response_started(getattr(self, "command", None), getattr(self, "path", None), code)
        super().send_response(code, message)

    def reply(self, status, body=None):
        payload = b"" if body is None else json.dumps(body, separators=(",", ":")).encode()
        if len(payload) > 131072:
            raise Refused("loopback response bound")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        self.wfile.write(payload)
        self.wfile.flush()

    def allowed(self):
        return (not self.server.stopped.is_set() and time.monotonic() < self.server.deadline
                and self.headers.get("Authorization") == "Bearer " + TOKEN)

    def wait(self):
        while not self.server.stopped.wait(0.025):
            if time.monotonic() >= self.server.deadline:
                return

    def do_GET(self):
        if not self.allowed():
            return self.reply(401)
        if self.path == "/v1/me":
            return self.reply(200, {
                "account_id": "synthetic-route-account", "device_id": "synthetic-route-device",
                "email": "synthetic@example.invalid", "plan": "pro", "protocol": 0,
                "keeper_url": self.server.origin, "hours_exhausted": False,
                "limits": {"cloud_hours": 1, "storage_bytes": 1},
                "usage": {"cloud_hours": 0.0, "storage_bytes": 0},
            })
        if self.path == "/v1/ssh/auth/capabilities":
            return self.reply(200, {
                "version": 1, "hostbound_v1": True, "register_only_v1": True,
                "proxyjump_v1": True, "route_policy_v1": True, "keeper_boot": BOOT,
            })
        if self.path != GRANT_PATH + "/" + GRANT + "/ws":
            return self.reply(404)
        if self.server.grants != 1:
            return self.reply(409)
        key = self.headers.get("Sec-WebSocket-Key", "")
        if (self.headers.get("Upgrade", "").lower() != "websocket"
                or len(key) != 24 or len(base64.b64decode(key, validate=True)) != 16):
            return self.reply(400)
        self.send_response(101)
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", base64.b64encode(hashlib.sha1(
            (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode())
        self.end_headers()
        self.close_connection = True
        self.server.ready_entered.set()
        if self.server.action == "ready-expiry":
            self.wait()
            return
        with self.server.lock:
            self.frame(1, json.dumps({
                "type": "ready", "version": 1, "grant_id": GRANT,
                "keeper_boot": BOOT, "legs": 1,
            }, separators=(",", ":")).encode())
            self.server.ready += 1
        # No sign request is fabricated. Keep the real native pump alive until
        # its owner closes it; accept only bounded WS control/close traffic.
        while not self.server.stopped.is_set() and time.monotonic() < self.server.deadline:
            if not select.select([self.connection], [], [], 0.025)[0]:
                continue
            head = self.rfile.read(2)
            if not head:
                return
            if len(head) != 2 or head[0] not in (0x88, 0x89, 0x8A) or not head[1] & 128:
                raise Refused("unexpected simulated control traffic")
            length = head[1] & 127
            if length > 125:
                raise Refused("simulated control frame bound")
            mask = self.rfile.read(4)
            data = self.rfile.read(length)
            if len(mask) != 4 or len(data) != length:
                raise Refused("partial simulated control frame")
            if head[0] == 0x88:
                return
            if head[0] == 0x89:
                self.frame(10, bytes(byte ^ mask[i % 4] for i, byte in enumerate(data)))

    def frame(self, opcode, payload):
        if len(payload) > 65535:
            raise Refused("simulated websocket output bound")
        prefix = bytes([128 | opcode])
        prefix += bytes([len(payload)]) if len(payload) < 126 else b"\x7e" + struct.pack("!H", len(payload))
        self.wfile.write(prefix + payload)
        self.wfile.flush()

    def do_POST(self):
        if not self.allowed():
            return self.reply(401)
        if self.path == "/v1/hosts/fixture-host/reconnect":
            with self.server.lock:
                if (self.server.ready != 1 or self.server.grants != 1
                        or self.headers.get("x-chimaera-ssh-auth-route-grant") != GRANT):
                    return self.reply(409)
                self.server.reconnects += 1
            return self.reply(204)
        if self.path != GRANT_PATH:
            return self.reply(404)
        raw_length = self.headers.get("Content-Length", "")
        if not raw_length.isdecimal() or not 1 <= int(raw_length) <= 131072:
            return self.reply(400)
        body = self.rfile.read(int(raw_length))
        if len(body) != int(raw_length):
            return self.reply(400)
        request = json.loads(body)
        legs = request.get("legs", [])
        if (request.get("version") != 1 or request.get("keeper_boot") != BOOT
                or request.get("destination") != DESTINATION
                or request.get("route") != {"version": 1, "jumps": []}
                or len(legs) != 1 or legs[0].get("destination") != DESTINATION
                or legs[0].get("mode") != "key"
                or legs[0].get("user_keys") != [self.server.public]
                or legs[0].get("host_keys") != [{"key": self.server.host, "is_ca": False}]
                or not isinstance(legs[0].get("policy"), dict)):
            raise Refused("configured identity grant mismatch")
        with self.server.lock:
            self.server.grants += 1
            if self.server.grants != 1:
                raise Refused("grant replay")
        self.server.grant_entered.set()
        if self.server.action in ("grant-expiry", "grant-cancel"):
            self.wait()
            return
        return self.reply(201, {
            "version": 1, "grant_id": GRANT, "expires_in": 180,
            "destination": request["destination"], "route": request["route"],
            "modes": [leg["mode"] for leg in legs], "policies": [leg["policy"] for leg in legs],
        })

    def do_DELETE(self):
        if not self.allowed() or self.path != GRANT_PATH + "/" + GRANT:
            return self.reply(401)
        with self.server.lock:
            self.server.deletes += 1
        return self.reply(204)


class RouteFixture(Fixture):
    def __init__(self, binary):
        self.protocols = []
        super().__init__(binary)

    def close(self):
        errors = []
        facts = []
        for helper in reversed(self.protocols):
            try:
                helper.close()
            except BaseException as error:
                if len(facts) < 6:
                    facts.append({"phase": "protocol-close", **failure_fact(error)})
                if not errors:
                    errors.append(error)
        # Retire processes/directories even if one listener refuses cleanup.
        try:
            super().close()
        except BaseException as error:
            if len(facts) < 6:
                facts.append({"phase": "fixture-close", **failure_fact(error)})
            if not errors:
                errors.append(error)
        if errors:
            diagnostic_print("DIAGNOSTIC route retained_cleanup", json.dumps(facts, separators=(",", ":")))
            raise errors[0]

    def run(self):
        encrypted, public = self.key("selected")
        _, host = self.key("host", passphrase="")
        foreign_key, _ = self.key("foreign", passphrase="")
        foreign_socket = self.root / "foreign-agent"
        self.spawn(["/usr/bin/ssh-agent", "-D", "-P", "", "-a", str(foreign_socket)], output=False)
        until = self.end(2)
        while not foreign_socket.exists() and time.monotonic() < until:
            time.sleep(0.02)
        env = dict(self.env, SSH_AUTH_SOCK=str(foreign_socket), SSH_ASKPASS_REQUIRE="never")
        if self.collect(["/usr/bin/ssh-add", str(foreign_key)], env)[0]:
            raise Refused("foreign seed refused")
        before = self.identities(env)
        known = self.root / "known"
        known.write_text("fixture.example.invalid ssh-ed25519 " + host.decode() + "\n")
        os.chmod(known, 0o600)
        config = self.root / "config"
        text = ("Host route-fixture\n HostName fixture.example.invalid\n User fixture\n Port 22\n"
                " IdentityAgent none\n IdentitiesOnly yes\n IdentityFile " + str(encrypted) + "\n"
                " UserKnownHostsFile " + str(known) + "\n GlobalKnownHostsFile none\n"
                " PubkeyAuthentication yes\n PasswordAuthentication no\n KbdInteractiveAuthentication no\n"
                " GSSAPIAuthentication no\n HostbasedAuthentication no\n"
                " PreferredAuthentications publickey\n HostKeyAlgorithms ssh-ed25519\n"
                " PubkeyAcceptedAlgorithms ssh-ed25519\n CASignatureAlgorithms ssh-ed25519\n"
                " ControlMaster no\n ControlPath none\n")
        for action in ("accept", "grant-expiry", "grant-cancel", "ready-expiry", "config-change"):
            config.write_text(text)
            os.chmod(config, 0o600)
            helper = Protocol(self, action, public.decode(), host.decode())
            helper.start()
            receipt = None
            answered = cancelled = False
            start = time.monotonic()
            app = None
            passed = False
            output = bytearray()
            facts = CaseFacts(start)
            original_error = None
            try:
                facts.mark("app-spawn")
                app = self.spawn([str(self.binary), "--route-fixture", str(config), helper.origin, action],
                                 env, input_pipe=True)

                def observed(output):
                    nonlocal receipt, answered
                    if b"ROUTE_PROMPT\n" in output and receipt is None:
                        facts.mark("helper-receipt")
                        receipt = self.helper_receipt(app)
                        facts.receipt_captured = True
                        facts.member_count = min(16, len(receipt[4]))
                        facts.mark("receipt-complete")
                    if receipt is not None and not answered:
                        if action == "config-change":
                            config.write_text(text.replace("fixture.example.invalid", "changed.example.invalid"))
                        # A bounded delay at unlock proves the later eight-second
                        # deadline includes time already spent before the grant.
                        facts.mark("answer-delay")
                        time.sleep(1)
                        facts.mark("answer-write")
                        if os.write(app.stdin.fileno(), b"CONTINUE\n") != 9:
                            raise Refused("route prompt handoff")
                        answered = True
                        facts.continue_sent = True
                        facts.mark("answer-sent")
                        if action != "grant-cancel":
                            facts.mark("answer-close")
                            app.stdin.close()
                            facts.stdin_closed = True
                            facts.mark("answer-closed")

                until = self.end(11)
                while True:
                    if time.monotonic() >= until:
                        raise Refused("route case deadline")
                    if action == "grant-cancel" and helper.grant_entered.is_set() and not cancelled:
                        facts.mark("cancel-write")
                        if os.write(app.stdin.fileno(), b"CANCEL\n") != 7:
                            raise Refused("route cancellation handoff")
                        cancelled = True
                        app.stdin.close()
                        facts.stdin_closed = True
                    if not select.select([app.stdout], [], [], 0.025)[0]:
                        continue
                    chunk = os.read(app.stdout.fileno(), 4096)
                    if not chunk:
                        break
                    if len(output) + len(chunk) > 8192:
                        raise Refused("route case output bound")
                    output.extend(chunk)
                    observed(output)
                facts.mark("app-wait")
                app.wait(timeout=max(0.01, until - time.monotonic()))
                if app.returncode != 0 or receipt is None or helper.failed:
                    raise Refused("route case refused")
                if action == "accept":
                    if b"ROUTE_ACCEPTED\n" not in output or (helper.grants, helper.ready, helper.reconnects, helper.deletes) != (1, 1, 1, 1):
                        raise Refused("positive ordered grant receipt")
                elif action == "config-change":
                    if b"ROUTE_REFUSED config\n" not in output or helper.grants or helper.reconnects:
                        raise Refused("config revalidation escaped")
                else:
                    expected = b"ROUTE_REFUSED revoked\n" if action == "grant-cancel" else b"ROUTE_REFUSED expiry\n"
                    if expected not in output or helper.grants != 1 or helper.reconnects:
                        raise Refused("late grant or reconnect escaped")
                    if action == "ready-expiry" and (not helper.ready_entered.is_set() or helper.deletes != 1):
                        raise Refused("ready deadline cleanup receipt")
                    if action.endswith("expiry") and time.monotonic() - start > 10:
                        raise Refused("original route deadline restarted")
                facts.mark("receipt-cleanup")
                self.cleanup_receipt(receipt)
                facts.mark("foreign-check")
                if self.identities(env) != before:
                    raise Refused("foreign fixture agent changed")
                facts.mark("case-complete")
                passed = True
                print("PASS", action, "original_owner configured_identity owned_cleanup foreign_unchanged", flush=True)
            except BaseException as error:
                original_error = error
                facts.original_error = {"phase": facts.phase, **failure_fact(error)}
                raise
            finally:
                # Record the original outcome before any retirement can cause
                # -SIGKILL or mask the first exception. These are observations,
                # not proof that stop_owned actually sent a signal.
                facts.pre_cleanup_exit = app.poll() if app is not None else None
                cleanup_errors = []
                for phase, operation in (
                    ("stdin-close", lambda: app.stdin.close() if app is not None and not app.stdin.closed else None),
                    ("protocol-close", helper.close),
                    ("app-stop", lambda: stop_owned(app) if app is not None else None),
                ):
                    try:
                        if phase == "app-stop" and app is not None:
                            facts.stop_owned_entered = True
                            facts.stop_owned_live_before = app.poll() is None
                        operation()
                        if phase == "stdin-close" and app is not None:
                            facts.stdin_closed = app.stdin.closed
                        if phase == "app-stop" and app is not None:
                            facts.stop_owned_returned = True
                    except BaseException as error:
                        cleanup_errors.append(error)
                        facts.cleanup_error(phase, error)
                if helper.failed:
                    error = Refused("loopback protocol owner failed")
                    cleanup_errors.append(error)
                    facts.cleanup_error("protocol-failed", error)
                if not passed or original_error is not None or cleanup_errors:
                    # Closed synthetic stage/counter facts only; never publish
                    # child bytes, request bodies, credentials or stderr.
                    diagnostic_print("DIAGNOSTIC route", json.dumps({
                        "action": action, "exit_code": app.returncode if app is not None else None,
                        "source_diagnostic": facts.fact(),
                        "prompt": b"ROUTE_PROMPT\n" in output,
                        "selected": b"ROUTE_SELECTED\n" in output,
                        "failed_marker": b"ROUTE_FAILED\n" in output,
                        "capabilities_accepted": b"ROUTE_STAGE capabilities\n" in output,
                        "selection_outcomes": [name for name in ("success", "unsupported_configuration",
                            "agent_unavailable", "no_keys", "too_many_keys", "host_trust_required",
                            "revoked_host", "unavailable")
                            if ("ROUTE_SELECTION " + name + "\n").encode("ascii") in output],
                        "requests": helper.requests, "grants": helper.grants, "ready": helper.ready,
                        "reconnects": helper.reconnects, "deletes": helper.deletes,
                        "protocol_failed": helper.failed, "failure_kind": helper.failure_kind,
                        "source_lines": helper.failure_lines,
                        "native_phases": native_phases(output),
                        "response_headers_started": helper.response_headers_started,
                        "response_overflow": helper.response_overflow,
                    }, separators=(",", ":")))
                if original_error is None and cleanup_errors:
                    raise cleanup_errors[0]
        print("PASS route5; simulated keeper protocol, no signing/SSH/Tauri/job acceptance", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    args = parser.parse_args()
    if sys.platform != "darwin" or not args.binary.is_absolute():
        raise Refused("Mac fixture and absolute binary required")
    import stat
    info = args.binary.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
        raise Refused("fixture binary ownership")

    def interrupted(*_):
        raise Refused("route fixture interrupted or expired")

    for name in (signal.SIGALRM, signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(name, interrupted)
    signal.setitimer(signal.ITIMER_REAL, 60)
    fixture = RouteFixture(args.binary)
    original_error = None
    try:
        fixture.run()
    except BaseException as error:
        original_error = error
        raise
    finally:
        # Cleanup uses only fixed, independently bounded owned-process receipts;
        # a second signal must not interrupt retirement midway through a group.
        signal.setitimer(signal.ITIMER_REAL, 0)
        for name in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            signal.signal(name, signal.SIG_IGN)
        try:
            fixture.close()
        except BaseException as error:
            diagnostic_print("DIAGNOSTIC route outer_cleanup", json.dumps({
                "original_error": failure_fact(original_error) if original_error is not None else None,
                "cleanup_error": failure_fact(error),
            }, separators=(",", ":")))
            if original_error is None:
                raise


if __name__ == "__main__":
    try:
        main()
    except (Refused, OSError, ValueError, subprocess.TimeoutExpired) as error:
        print("FAIL route", json.dumps(failure_fact(error), separators=(",", ":")), file=sys.stderr)
        sys.exit(1)
