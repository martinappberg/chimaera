#!/usr/bin/env python3
"""Opt-in Linux Claude compatibility fixture; never an installed launch path.

The outer VM owner holds the shared lock through namespace exit. Only --outer is
an entrypoint; private roles require a distinct network namespace, no capabilities
and the canary receipts. Reports contain synthetic counts, never raw CLI output.
"""
import argparse
import fcntl
import hashlib
import http.client
import http.server
import json
import os
import selectors
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
import threading
from contextlib import closing

CLI_SHA = "e4daf793d1e74fb0d9874dd09e98690bbfd7be515f78a87fd05b9e2b4bb33b03"
LOCK = "/run/chimaera-project-isolation-fixture.lock"
LIMIT = 128 * 1024
BWRAP_PHASES = frozenset(
    "bwrap-" + stage + "-" + cause
    for stage in ("setup", "exec", "source", "unknown")
    for cause in ("denied", "missing", "invalid", "other")
)

# Only fixed fixture phases cross process boundaries; no exception text, CLI
# output, request bodies or credentials may become a diagnostic.
REFUSAL_PREFIX = "contained Claude probe refused: "
REFUSAL_PHASES = frozenset((
    'actual-tui-count-tokens',
    'artifact-hash',
    'artifact-selector',
    'artifact-type',
    'census-size',
    'child-deadline',
    'child-exit-deadline',
    'child-output',
    'child-spawn',
    'descendant-external-canary',
    'descendant-ipv6-canary',
    'drop-privileges',
    'external-canary',
    'group-cleanup',
    'inherited-fd-count',
    'inherited-network-fd',
    'interrupted',
    'ipv6-external-canary',
    'lock-type',
    'loopback-setup',
    'namespace-identity',
    'namespace-links',
    'namespace-routes',
    'namespace-test-receipt',
    'namespace-test-result',
    'namespace-uid',
    'official-cli-version',
    'official-header-receipt',
    'official-tests',
    'outer-address',
    'outer-canary-reached',
    'outer-platform',
    'role',
    'root-identity',
    'run-caps',
    'run-namespace',
    'run-no-new-privs',
    'run-status',
    'system-symlink',
    'tui-admission',
    'tui-auth',
    'tui-body-bound',
    'tui-complete-body',
    'tui-header-bound',
    'tui-length',
    'tui-messages-receipt',
    'tui-no-other-auth',
    'tui-output-bound',
    'tui-recorder-cleanup',
    'tui-reply-bound',
    'tui-request-count',
    'tui-route',
    'tui-target',
    'unexpected',
)) | BWRAP_PHASES


class Refusal(RuntimeError):
    def __init__(self, phase):
        self.phase = phase if phase in REFUSAL_PHASES else "unexpected"
        super().__init__("contained probe refused")


def child_refusal(errors):
    # A nested role may report its fixed phase; raw diagnostics stay private.
    for line in errors.splitlines():
        if line.startswith(REFUSAL_PREFIX.encode("ascii")):
            phase = line[len(REFUSAL_PREFIX):].decode("ascii", errors="replace")
            if phase in REFUSAL_PHASES:
                return phase
    return None


def bwrap_refusal(errors):
    # Diagnostics classify a failed fixed launcher, never prove its cause or
    # forward a pathname. Unknown Bubblewrap wording stays a fixed category.
    for line in errors.splitlines():
        if not line.startswith(b"bwrap: "):
            continue
        message = line[len(b"bwrap: "):]
        if message.startswith((b"execvp ", b"execv ", b"Can't exec ")):
            stage = "exec"
        elif message.startswith((b"Can't find source path ", b"Can't open ",
                                 b"Can't stat ", b"Invalid fd ")):
            stage = "source"
        elif message.startswith((b"Can't mkdir ", b"Can't mount ", b"Can't bind mount ",
                                 b"Can't create ", b"Can't chdir ",
                                 b"Creating new namespace failed", b"Setting up uid map",
                                 b"Setting up gid map", b"Failed to ", b"No permissions to ")):
            stage = "setup"
        else:
            stage = "unknown"
        if message.endswith((b": Permission denied", b": Operation not permitted")):
            cause = "denied"
        elif message.endswith(b": No such file or directory"):
            cause = "missing"
        elif message.endswith((b": Invalid argument", b": Bad file descriptor",
                               b": Not a directory")):
            cause = "invalid"
        else:
            cause = "other"
        return "bwrap-" + stage + "-" + cause
    return "bwrap-unknown-other"


def require(condition, phase):
    if not condition:
        raise Refusal(phase)


def json_command(args):
    code, output, errors = bounded_child(args, {"PATH": "/usr/bin:/bin"}, 3)
    require(code == 0 and len(output) <= 65536 and errors <= 4096, "census-size")
    return json.loads(output)


def bounded_child(args, env, timeout, pass_fds=(), cwd=None):
    """Own all output and actual group cleanup even on assertion/interruption."""
    original_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
    process = None
    read = selectors.DefaultSelector()
    buffers = [bytearray(), bytearray()]
    try:
        process = subprocess.Popen(args, env=env, cwd=cwd, pass_fds=pass_fds,
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, start_new_session=True)
        signal.pthread_sigmask(signal.SIG_SETMASK, original_mask)
        for index, pipe in enumerate((process.stdout, process.stderr)):
            os.set_blocking(pipe.fileno(), False)
            read.register(pipe, selectors.EVENT_READ, index)
        end = time.monotonic() + timeout
        while read.get_map():
            require(time.monotonic() < end, "child-deadline")
            for key, _ in read.select(min(.1, max(0, end - time.monotonic()))):
                block = os.read(key.fileobj.fileno(), 8192)
                if not block:
                    read.unregister(key.fileobj)
                else:
                    target = buffers[key.data]
                    require(len(target) + len(block) <= LIMIT, "child-output")
                    target.extend(block)
        # Observe exit without reaping; the original PID cannot be reused before
        # the final group signal. Never signal a PID after releasing that receipt.
        while True:
            receipt = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            if receipt is not None:
                code = receipt.si_status if receipt.si_code == os.CLD_EXITED else -receipt.si_status
                if code != 0:
                    phase = child_refusal(buffers[1])
                    if phase is not None:
                        raise Refusal(phase)
                    if args[0] == "/usr/bin/bwrap":
                        raise Refusal(bwrap_refusal(buffers[1]))
                return code, bytes(buffers[0]), len(buffers[1])
            require(time.monotonic() < end, "child-exit-deadline")
            time.sleep(.01)
    finally:
        signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
        read.close()
        if process is None:
            signal.pthread_sigmask(signal.SIG_SETMASK, original_mask)
            raise Refusal("child-spawn")
        # The outer PID namespace additionally owns descendants that leave this
        # group. A result is never published before the direct group disappears.
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.killpg(process.pid, sig)
            except ProcessLookupError:
                break
            until = time.monotonic() + 2
            while time.monotonic() < until:
                if os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None:
                    break
                time.sleep(.01)
            else:
                continue
            # Kill remaining group members while the unreaped leader still pins
            # this group identity, including after an ordinary successful exit.
            os.killpg(process.pid, signal.SIGKILL)
            break
        process.wait(timeout=2)
        for pipe in (process.stdout, process.stderr):
            pipe.close()
        until = time.monotonic() + 2
        while True:
            try:
                os.killpg(process.pid, 0)
            except ProcessLookupError:
                break
            require(time.monotonic() < until, "group-cleanup")
            time.sleep(.01)
        signal.pthread_sigmask(signal.SIG_SETMASK, original_mask)


def pinned_file(path, expected):
    require(path.startswith("/fixtures/") and len(expected) == 64, "artifact-selector")
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
    try:
        metadata = os.fstat(fd)
        require(stat.S_ISREG(metadata.st_mode) and metadata.st_size <= 512 * 1024 * 1024,
                "artifact-type")
        digest = hashlib.sha256()
        while True:
            data = os.read(fd, 65536)
            if not data:
                break
            digest.update(data)
        require(digest.hexdigest() == expected, "artifact-hash")
        os.lseek(fd, 0, os.SEEK_SET)
        return fd
    except BaseException:
        os.close(fd)
        raise


def inner_prepare():
    require(os.geteuid() == 0, "namespace-uid")
    require(os.stat("/proc/self/ns/net").st_ino != int(os.environ["PROBE_OUTER_NET"]),
            "namespace-identity")
    descriptors = os.listdir("/proc/self/fd")
    require(len(descriptors) <= 32, "inherited-fd-count")
    for fd in descriptors:
        try:
            target = os.readlink("/proc/self/fd/" + fd)
        except FileNotFoundError:
            # listdir's own temporary directory descriptor is already closed.
            continue
        require(not target.startswith("socket:"), "inherited-network-fd")
    try:
        subprocess.run(["/usr/sbin/ip", "link", "set", "lo", "up"], timeout=3, check=True)
    except (OSError, subprocess.SubprocessError):
        raise Refusal("loopback-setup") from None
    links = json_command(["/usr/sbin/ip", "-j", "link", "show"])
    require(len(links) == 1 and links[0]["ifname"] == "lo", "namespace-links")
    for version in ("-4", "-6"):
        routes = json_command(["/usr/sbin/ip", version, "-j", "route", "show", "table", "all"])
        require(all(row.get("dev") == "lo" for row in routes), "namespace-routes")
    # Successful outer traffic would be observable by this retained listener.
    with socket.socket() as blocked:
        blocked.settimeout(.3)
        require(blocked.connect_ex((os.environ["PROBE_OUTER_IP"],
                                    int(os.environ["PROBE_OUTER_PORT"]))) != 0,
                "external-canary")
    with socket.socket(socket.AF_INET6) as blocked6:
        blocked6.settimeout(.3)
        require(blocked6.connect_ex(("2001:db8::1", 443)) != 0, "ipv6-external-canary")
    # Raise loopback only, then irreversibly remove namespace/network privileges
    # before a test or official CLI can execute. The next role repeats canaries.
    env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "HOME": "/tmp",
           "PROBE_OUTER_NET": os.environ["PROBE_OUTER_NET"],
           "PROBE_OUTER_IP": os.environ["PROBE_OUTER_IP"],
           "PROBE_OUTER_PORT": os.environ["PROBE_OUTER_PORT"]}
    try:
        os.execve("/usr/bin/setpriv", ["setpriv", "--bounding-set=-all", "--inh-caps=-all",
                  "--ambient-caps=-all", "--no-new-privs", "/usr/bin/python3", "-I",
                  "/probe/wrapper.py", "--inner-run"], env)
    except OSError:
        raise Refusal("drop-privileges") from None


def inner_run():
    require(os.stat("/proc/self/ns/net").st_ino != int(os.environ["PROBE_OUTER_NET"]),
            "run-namespace")
    status = open("/proc/self/status", encoding="ascii").read(65537)
    require(len(status) <= 65536, "run-status")
    fields = dict(line.split(":", 1) for line in status.splitlines() if ":" in line)
    require(all(int(fields[name].strip(), 16) == 0
                for name in ("CapEff", "CapPrm", "CapInh", "CapAmb", "CapBnd")), "run-caps")
    require(fields["NoNewPrivs"].strip() == "1", "run-no-new-privs")
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        with socket.create_connection(listener.getsockname(), timeout=.3):
            connection, _ = listener.accept()
            connection.close()
    with socket.socket() as blocked:
        blocked.settimeout(.3)
        require(blocked.connect_ex((os.environ["PROBE_OUTER_IP"],
                                    int(os.environ["PROBE_OUTER_PORT"]))) != 0,
                "descendant-external-canary")
    with socket.socket(socket.AF_INET6) as blocked6:
        blocked6.settimeout(.3)
        require(blocked6.connect_ex(("2001:db8::1", 443)) != 0, "descendant-ipv6-canary")
    env = {"HOME": "/tmp", "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8",
           "CHIMAERA_TEST_CLAUDE_21287_CONTAINED": "1"}
    code, version, _ = bounded_child(["/usr/local/bin/claude", "--version"], env, 5)
    require(code == 0 and version.startswith(b"2.1.287 "), "official-cli-version")
    code, output, stderr_size = bounded_child(
        ["/probe/server-tests", "provider_claude_child::official_tests::",
         "--ignored", "--test-threads=1", "--nocapture"], env, 80)
    require(code == 0 and b"2 passed" in output, "official-tests")
    headers = [json.loads(line.split("=", 1)[1]) for line in output.decode("utf-8").splitlines()
               if line.startswith("CLAUDE_PROBE_HEADERS=")]
    require(len(headers) == 1, "official-header-receipt")
    print(json.dumps({"version": "2.1.287", "namespace_canaries": True,
                      "official_tests": 2, "stderr_bytes": stderr_size,
                      "headers": headers[0],
                      "private_broker": False, "runtime_enabled": False}))


def outer(args):
    require(sys.platform == "linux" and os.geteuid() == 0, "outer-platform")
    lock = os.open(LOCK, os.O_CREAT | os.O_RDONLY | os.O_NOFOLLOW |
                   os.O_CLOEXEC | os.O_NONBLOCK, 0o600)
    root = None
    fds = []
    try:
        metadata = os.fstat(lock)
        require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid == 0 and
                metadata.st_nlink == 1 and stat.S_IMODE(metadata.st_mode) == 0o600, "lock-type")
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        root = tempfile.mkdtemp(prefix="chimaera-claude-probe-", dir="/var/lib")
        identity = os.stat(root)
        os.mkdir(root + "/etc")
        os.mkdir(root + "/tmp")
        os.chmod(root + "/tmp", 0o1777)
        for name, data in (("passwd", "root:x:0:0:probe:/tmp:/bin/false\n"),
                           ("group", "root:x:0:\n"),
                           ("hosts", "127.0.0.1 localhost\n::1 localhost\n")):
            with open(root + "/etc/" + name, "x", encoding="ascii") as file:
                file.write(data)
        test = pinned_file(args.test_binary, args.test_sha256)
        cli = pinned_file("/fixtures/claude-2.1.287/claude", CLI_SHA)
        source = pinned_file(args.wrapper, args.wrapper_sha256)
        fds.extend((test, cli, source))
        addresses = json_command(["/usr/sbin/ip", "-4", "-j", "addr", "show"])
        external = [entry["local"] for row in addresses if row["ifname"] != "lo"
                    for entry in row.get("addr_info", []) if entry["family"] == "inet"]
        require(bool(external), "outer-address")
        with socket.socket() as canary:
            canary.bind((external[0], 0))
            canary.listen(4)
            command = ["/usr/bin/bwrap", "--die-with-parent", "--unshare-pid",
                       "--unshare-net", "--cap-drop", "ALL", "--cap-add", "CAP_NET_ADMIN",
                       # CAP_SETPCAP is setup-only: setpriv needs it to empty
                       # the bounding set. inner-run verifies every set is zero.
                       "--cap-add", "CAP_SETPCAP",
                       "--ro-bind", "/usr", "/usr", "--ro-bind", root + "/etc", "/etc",
                       "--bind", root + "/tmp", "/tmp", "--tmpfs", "/run",
                       "--dev", "/dev", "--proc", "/proc", "--dir", "/probe"]
            if os.path.islink("/usr/local"):
                require(os.readlink("/usr/local") == "../var/usrlocal", "system-symlink")
                # CoreOS points here outside the read-only /usr image. Supply
                # empty private targets, never the host's mutable /var tree.
                command.extend(("--dir", "/var", "--dir", "/var/usrlocal",
                                "--dir", "/var/usrlocal/bin"))
            else:
                require(os.path.isdir("/usr/local"), "system-symlink")
            command.extend(("--tmpfs", "/usr/local/bin",
                       "--ro-bind-fd", str(test), "/probe/server-tests",
                       "--ro-bind-fd", str(cli), "/usr/local/bin/claude",
                       "--ro-bind-fd", str(source), "/probe/wrapper.py"))
            for name in ("bin", "sbin", "lib", "lib64"):
                if os.path.islink("/" + name):
                    target = os.readlink("/" + name)
                    require(target in ("usr/" + name, "/usr/" + name), "system-symlink")
                    command.extend(("--symlink", target, "/" + name))
                elif os.path.isdir("/" + name):
                    command.extend(("--ro-bind", "/" + name, "/" + name))
            command.extend(("--", "/usr/bin/python3", "-I", "/probe/wrapper.py", "--inner-prepare"))
            env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8",
                   "PROBE_OUTER_NET": str(os.stat("/proc/self/ns/net").st_ino),
                   "PROBE_OUTER_IP": external[0], "PROBE_OUTER_PORT": str(canary.getsockname()[1])}
            code, report, _ = bounded_child(command, env, 100, tuple(fds))
            canary.setblocking(False)
            try:
                accepted, _ = canary.accept()
            except BlockingIOError:
                pass
            else:
                accepted.close()
                raise Refusal("outer-canary-reached")
            require(code == 0, "namespace-test-result")
            value = json.loads(report)
            require(value.get("official_tests") == 2, "namespace-test-receipt")
            print(json.dumps(value))
        # Successful bwrap/PID namespace exit is required before removing own
        # evidence. On failure keep the root for diagnosis; never broad cleanup.
        current = os.stat(root)
        require((current.st_dev, current.st_ino) == (identity.st_dev, identity.st_ino), "root-identity")
        shutil.rmtree(root)
    finally:
        for fd in fds:
            os.close(fd)
        os.close(lock)


def seed_synthetic_workspace_trust():
    """Only this fresh synthetic fixture owns the original inherited config FD.

    The compatibility gate does not exercise onboarding/trust UX. No path or
    project selector is accepted: cwd already came from the checked Rust owner.
    The renamed original directory receives the decision, never its replacement.
    """
    value = os.environ.get("CLAUDE_CONFIG_DIR", "")
    prefix = "/proc/self/fd/"
    require(value.startswith(prefix) and value[len(prefix):].isdigit(), "tui-admission")
    descriptor = int(value[len(prefix):])
    require(descriptor >= 3, "tui-admission")
    directory = os.fstat(descriptor)
    require(stat.S_ISDIR(directory.st_mode) and directory.st_uid == os.geteuid() and
            directory.st_mode & 0o022 == 0, "tui-admission")
    fd = None
    try:
        fd = os.open(".claude.json", os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC,
                     dir_fd=descriptor)
        metadata = os.fstat(fd)
        require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid == os.geteuid() and
                metadata.st_nlink == 1 and metadata.st_mode & 0o022 == 0 and
                metadata.st_size <= 4096, "tui-admission")
        raw = bytearray()
        while True:
            block = os.read(fd, min(4097 - len(raw), 4096))
            if not block:
                break
            raw.extend(block)
            require(len(raw) <= 4096, "tui-admission")
        config = json.loads(raw)
        # Refuse any populated/shared CLI configuration rather than importing it.
        require(isinstance(config, dict) and
                set(config) == {"hasCompletedOnboarding", "bypassPermissionsModeAccepted"} and
                config["hasCompletedOnboarding"] is True and
                config["bypassPermissionsModeAccepted"] is True, "tui-admission")
        cwd = os.getcwd()
        require(cwd.startswith("/") and cwd != os.environ["HOME"] and
                len(cwd.encode("utf-8")) <= 4096, "tui-admission")
        config["projects"] = {cwd: {"hasTrustDialogAccepted": True}}
        data = json.dumps(config, separators=(",", ":")).encode("utf-8")
        require(len(data) <= 4096, "tui-admission")
        os.lseek(fd, 0, os.SEEK_SET)
        written = 0
        while written < len(data):
            count = os.write(fd, data[written:])
            require(count > 0, "tui-admission")
            written += count
        os.ftruncate(fd, len(data))
        os.fsync(fd)
        return descriptor
    except (OSError, ValueError, KeyError, TypeError):
        raise Refusal("tui-admission") from None
    finally:
        if fd is not None:
            os.close(fd)


def tui_child():
    """Contained actual PTY + local header recorder; no generic upstream proxy."""
    import pty
    import select
    import termios
    import struct
    require(os.environ.get("CHIMAERA_TEST_CLAUDE_21287_CONTAINED") == "1", "tui-admission")
    from urllib.parse import urlsplit
    target = urlsplit(os.environ["ANTHROPIC_BASE_URL"])
    require(target.scheme == "http" and target.hostname == "127.0.0.1" and
            target.port is not None and target.path == "", "tui-target")
    config_fd = seed_synthetic_workspace_trust()
    routes = []
    safe_headers = []
    failed = []
    messages_complete = threading.Event()
    count_tokens_complete = threading.Event()

    class Recorder(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_args):
            pass

        def do_POST(self):
            try:
                require(self.path in ("/v1/messages?beta=true", "/v1/messages/count_tokens?beta=true"),
                        "tui-route")
                require(len(routes) < 16 and len(self.headers) <= 32, "tui-request-count")
                require(self.headers.get("Authorization") ==
                        "Bearer " + os.environ["CLAUDE_CODE_OAUTH_TOKEN"], "tui-auth")
                require(self.headers.get("x-api-key") is None and
                        self.headers.get("Transfer-Encoding") is None, "tui-no-other-auth")
                require(len(self.headers.get_all("Content-Length", [])) == 1, "tui-length")
                length = int(self.headers["Content-Length"])
                require(0 < length <= 1024 * 1024, "tui-body-bound")
                self.connection.settimeout(3)
                body = self.rfile.read(length)
                require(len(body) == length, "tui-complete-body")
                routes.append(self.path)
                recorded = {name: self.headers.get(name, "")
                            for name in ("anthropic-version", "anthropic-beta", "user-agent")}
                require(all(len(value) <= 512 and os.environ["CLAUDE_CODE_OAUTH_TOKEN"] not in value
                            for value in recorded.values()), "tui-header-bound")
                safe_headers.append(recorded)
                headers = dict(self.headers.items())
                headers["Host"] = "127.0.0.1:" + str(target.port)
                with closing(http.client.HTTPConnection("127.0.0.1", target.port, timeout=3)) as upstream:
                    upstream.request("POST", self.path, body, headers)
                    reply = upstream.getresponse()
                    value = reply.read(1024 * 1024 + 1)
                    require(len(value) <= 1024 * 1024, "tui-reply-bound")
                    self.send_response(reply.status)
                    self.send_header("Content-Type", reply.getheader("Content-Type", "application/json"))
                    self.send_header("Content-Length", str(len(value)))
                    self.send_header("Connection", "close")
                    self.end_headers()
                    self.wfile.write(value)
                    self.wfile.flush()
                    if self.path == "/v1/messages?beta=true":
                        require(reply.status == 200, "tui-messages-receipt")
                        if reply.getheader("Content-Type", "").startswith("text/event-stream"):
                            require(b'"SYNTHETIC_OK"' in value and
                                    b'"type":"message_stop"' in value.replace(b" ", b""),
                                    "tui-messages-receipt")
                        else:
                            message = json.loads(value)
                            require(message.get("role") == "assistant" and
                                    message.get("stop_reason") == "end_turn" and
                                    any(block.get("text") == "SYNTHETIC_OK"
                                        for block in message.get("content", [])),
                                    "tui-messages-receipt")
                        messages_complete.set()
                    else:
                        require(reply.status == 200 and
                                json.loads(value).get("input_tokens") == 1,
                                "actual-tui-count-tokens")
                        count_tokens_complete.set()
            except BaseException as error:
                failed.append(error.phase if isinstance(error, Refusal) else "unexpected")
                self.close_connection = True

    server = http.server.HTTPServer(("127.0.0.1", 0), Recorder)
    server.timeout = .1
    server_done = threading.Event()
    def serving():
        while not server_done.is_set():
            server.handle_request()
    thread = threading.Thread(target=serving)
    thread.start()
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 100, 0, 0))
    env = dict(os.environ)
    env["TERM"] = "xterm-256color"
    env["ANTHROPIC_BASE_URL"] = "http://127.0.0.1:" + str(server.server_port)
    process = None
    try:
        process = subprocess.Popen(["/usr/local/bin/claude", "Respond with the fixture's short success response.",
            "--no-session-persistence", "--setting-sources", "", "--tools", "",
            "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}'],
            env=env, stdin=slave, stdout=slave, stderr=slave, pass_fds=(config_fd,))
        os.close(slave)
        slave = -1
        output = bytearray()
        until = time.monotonic() + 20
        context_sent = False
        while time.monotonic() < until and process.poll() is None:
            if select.select([master], [], [], .1)[0]:
                try:
                    block = os.read(master, 8192)
                except OSError:
                    break
                require(len(output) + len(block) <= LIMIT, "tui-output-bound")
                output.extend(block)
                if b"\x1b[6n" in block:
                    os.write(master, b"\x1b[1;1R")
            # The marker is absent from the initial prompt. Both the completed
            # synthetic Messages reply and its actual rendered output must be
            # observed before issuing the context command.
            if not context_sent and messages_complete.is_set() and b"SYNTHETIC_OK" in output:
                os.write(master, b"/context\r")
                context_sent = True
            if failed:
                raise Refusal(failed[0])
            if count_tokens_complete.is_set():
                break
        require(not failed and context_sent and messages_complete.is_set() and
                count_tokens_complete.is_set(), "actual-tui-count-tokens")
    finally:
        if process is not None:
            process.terminate()
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=2)
        if slave >= 0:
            os.close(slave)
        os.close(master)
        server_done.set()
        thread.join(timeout=5)
        server.server_close()
        require(not thread.is_alive(), "tui-recorder-cleanup")
    print(json.dumps({"routes": routes, "headers": safe_headers, "auth_frontend_only": True}))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--outer", action="store_true")
    parser.add_argument("--inner-prepare", action="store_true")
    parser.add_argument("--inner-run", action="store_true")
    parser.add_argument("--tui-child", action="store_true")
    parser.add_argument("--test-binary")
    parser.add_argument("--test-sha256")
    parser.add_argument("--wrapper")
    parser.add_argument("--wrapper-sha256")
    args = parser.parse_args()
    require(sum((args.outer, args.inner_prepare, args.inner_run, args.tui_child)) == 1, "role")
    if args.outer:
        outer(args)
    elif args.inner_prepare:
        inner_prepare()
    elif args.inner_run:
        inner_run()
    else:
        tui_child()


if __name__ == "__main__":
    def interrupted(_signal, _frame):
        raise Refusal("interrupted")
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        main()
    except BaseException as error:
        phase = error.phase if isinstance(error, Refusal) else "unexpected"
        print(REFUSAL_PREFIX + phase, file=sys.stderr)
        sys.exit(1)
