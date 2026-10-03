#!/usr/bin/env python3
"""Bounded synthetic Mac loader proof; never reads a user's keys or agent.

Build the explicit ssh-agent-fixture target, then pass its absolute path with
--binary. The Rust target imports unchanged production modules; its askpass
transport is synthetic and does not certify the Tauri prompt UI.
"""

import argparse
import hashlib
import os
import pathlib
import select
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time


class Refused(Exception):
    """Fixed-category fixture failure, without private command diagnostics."""


def stop_owned(process):
    # Popen retains the child until wait; its new-session PGID is its own PID.
    if process.poll() is None:
        if os.getpgid(process.pid) != process.pid:
            raise Refused("owned process group changed")
        os.killpg(process.pid, signal.SIGKILL)
    process.wait(timeout=2)
    if process.stdin is not None:
        process.stdin.close()
    if process.stdout is not None:
        process.stdout.close()


def capture(process, deadline, limit, observed=None):
    result = bytearray()
    while True:
        if time.monotonic() >= deadline:
            raise Refused("fixed command deadline")
        if not select.select([process.stdout], [], [], 0.025)[0]:
            continue
        chunk = os.read(process.stdout.fileno(), 4096)
        if not chunk:
            break
        if len(result) + len(chunk) > limit:
            raise Refused("fixed command output bound")
        result.extend(chunk)
        if observed:
            observed(result)
    process.wait(timeout=max(0.01, deadline - time.monotonic()))
    return bytes(result)


class Fixture:
    def __init__(self, binary):
        self.binary = binary
        self.root = pathlib.Path(tempfile.mkdtemp(prefix="chimaera-loader-fixture-", dir="/private/tmp"))
        self.root_identity = self.root.stat().st_dev, self.root.stat().st_ino
        self.deadline = time.monotonic() + 60
        self.owned = []
        self.helpers = []
        # Never inherit DYLD, user ssh config, credential helpers, or user agents.
        self.env = {"PATH": "/usr/bin:/bin", "LANG": "C", "LC_ALL": "C", "HOME": str(self.root)}

    def end(self, seconds):
        return min(self.deadline, time.monotonic() + seconds)

    def spawn(self, argv, env=None, output=True, input_pipe=False):
        if time.monotonic() >= self.deadline or len(self.owned) >= 2048:
            raise Refused("fixture process budget")
        process = subprocess.Popen(
            argv,
            env=self.env if env is None else env,
            stdin=subprocess.PIPE if input_pipe else subprocess.DEVNULL,
            stdout=subprocess.PIPE if output else subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        self.owned.append(process)
        if os.getpgid(process.pid) != process.pid:
            stop_owned(process)
            raise Refused("owned session receipt")
        return process

    def collect(self, argv, env=None, limit=65536, seconds=5):
        process = self.spawn(argv, env)
        try:
            output = capture(process, self.end(seconds), limit)
            return process.returncode, output
        finally:
            stop_owned(process)

    def process_identity(self, pid):
        _, output = self.collect(
            ["/bin/ps", "-p", str(pid), "-o", "lstart=,stat=,pgid="], limit=256, seconds=1
        )
        fields = output.split()
        if not fields:
            return None
        if len(fields) != 7 or not fields[-1].isdigit():
            raise Refused("process identity shape")
        return b" ".join(fields[:5]), fields[5], int(fields[6])

    def helper_receipt(self, app):
        _, children = self.collect(["/usr/bin/pgrep", "-P", str(app.pid)], limit=8192, seconds=1)
        for child in children.split():
            if not child.isdigit():
                raise Refused("child receipt shape")
            pid = int(child)
            _, output = self.collect(
                ["/bin/ps", "-p", str(pid), "-o", "command="], limit=8192, seconds=1
            )
            # All helper argv are closed production values; no private payload.
            fields = output.decode("utf-8", "strict").split()
            if len(fields) != 4 or fields[0] != str(self.binary) or fields[1] != "--native-key-agent":
                continue
            path = pathlib.Path(fields[2])
            if path.parent != pathlib.Path("/private/tmp") or not path.name.startswith("cx-native-key-agent-"):
                raise Refused("helper directory receipt")
            info = path.lstat()
            identity = self.process_identity(pid)
            if (not stat.S_ISDIR(info.st_mode) or stat.S_IMODE(info.st_mode) != 0o700
                    or info.st_uid != os.getuid() or identity is None or identity[2] != pid):
                raise Refused("helper ownership receipt")
            _, output = self.collect(["/bin/ps", "-axo", "pid,pgid"], limit=262144, seconds=1)
            members = {}
            for row in output.splitlines()[1:]:
                member = row.split()
                if len(member) == 2 and member[0].isdigit() and member[1] == str(pid).encode():
                    found = self.process_identity(int(member[0]))
                    if found is not None:
                        members[int(member[0])] = found[0]
            if pid not in members or len(members) < 4 or len(members) > 16:
                raise Refused("helper descendant receipt")
            receipt = pid, identity[0], path, (info.st_dev, info.st_ino), members
            self.helpers.append(receipt)
            return receipt
        raise Refused("missing helper receipt")

    def cleanup_receipt(self, receipt):
        _, _, path, _, members = receipt
        deadline = self.end(3)
        while time.monotonic() < deadline:
            live = False
            for pid, start in members.items():
                identity = self.process_identity(pid)
                if identity is not None and identity[0] == start and not identity[1].startswith(b"Z"):
                    live = True
            if not live and not path.exists():
                return
            time.sleep(0.025)
        raise Refused("owned helper cleanup not positive")

    def key(self, name, kind="ed25519", passphrase="fixture-only-passphrase"):
        path = self.root / name
        argv = ["/usr/bin/ssh-keygen", "-q", "-t", kind, "-N", passphrase, "-C", "synthetic-only", "-f", str(path)]
        if kind == "rsa":
            argv += ["-b", "2048", "-m", "PEM"]
        if self.collect(argv)[0]:
            raise Refused("synthetic key generation")
        public_path = path.with_suffix(".pub")
        with public_path.open("rb") as source:
            public = source.read(8193)
        if len(public) > 8192 or len(public.split()) != 3:
            raise Refused("synthetic public identity")
        public_path.unlink()  # Acceptance must not depend on a .pub sidecar.
        return path, public.split()[1]

    def identities(self, env):
        code, public = self.collect(["/usr/bin/ssh-add", "-L"], env)
        if code:
            raise Refused("foreign identity receipt")
        return hashlib.sha256(public).digest()

    def run(self):
        encrypted, public = self.key("selected")
        pem, pem_public = self.key("pem", "rsa")
        foreign_key, _ = self.key("foreign", passphrase="")
        socket = self.root / "foreign-agent"
        self.spawn(["/usr/bin/ssh-agent", "-D", "-P", "", "-a", str(socket)], output=False)
        deadline = self.end(2)
        while not socket.exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        env = dict(self.env, SSH_AUTH_SOCK=str(socket), SSH_ASKPASS_REQUIRE="never")
        if self.collect(["/usr/bin/ssh-add", str(foreign_key)], env)[0]:
            raise Refused("foreign seed refused")
        before = self.identities(env)
        cases = [(encrypted, public, "accept"), (pem, pem_public, "accept")]
        cases += [(encrypted, None, action) for action in ("wrong", "decline", "cancel", "deadline", "pause")]
        for path, expected, action in cases:
            app = self.spawn([str(self.binary), "--key-load-fixture", str(path), action], env, input_pipe=True)
            receipt = None
            killed = False
            answered = 0

            def observed(output):
                nonlocal receipt, killed, answered
                if b"PROMPT\n" in output and receipt is None:
                    receipt = self.helper_receipt(app)
                if action == "pause" and receipt is not None and not killed:
                    # Kill only the app. The independently owned helper must
                    # observe its parent pipe EOF and retire its own group.
                    os.kill(app.pid, signal.SIGKILL)
                    killed = True
                elif action != "pause":
                    while answered < output.count(b"PROMPT\n"):
                        if os.write(app.stdin.fileno(), b"CONTINUE\n") != 9:
                            raise Refused("prompt ownership handoff")
                        answered += 1

            output = capture(app, self.end(8), 8192, observed)
            if expected is not None:
                if app.returncode != 0 or b"PUBLIC " + expected + b"\n" not in output:
                    raise Refused("positive key association")
            elif action == "pause":
                if not killed or app.returncode != -signal.SIGKILL:
                    raise Refused("app-death receipt")
            elif app.returncode != 2 or b"PUBLIC " in output:
                raise Refused("negative load accepted")
            if receipt is None:
                raise Refused("missing positive helper receipt")
            self.cleanup_receipt(receipt)
            if self.identities(env) != before:
                raise Refused("foreign fixture agent changed")
            print("PASS", path.name, action, "owned_cleanup foreign_unchanged", flush=True)
        print("PASS loader7; synthetic prompts, no Tauri UI or keeper grant acceptance", flush=True)

    def close(self):
        # Cleanup has its own finite window even after the workload deadline.
        self.deadline = time.monotonic() + 10
        for process in reversed(self.owned):
            stop_owned(process)
        # If a production cleanup assertion fails, retire only the still exact
        # recorded helper identity. Never glob, kill by UID, or unlink others.
        for pid, start, path, directory, _ in self.helpers:
            identity = self.process_identity(pid)
            if identity is not None and identity[0] == start and identity[2] == pid:
                os.killpg(pid, signal.SIGKILL)
            if path.exists():
                info = path.lstat()
                if (info.st_dev, info.st_ino) == directory:
                    for name in ("agent", "load", "askpass", "askpass.sh"):
                        (path / name).unlink(missing_ok=True)
                    path.rmdir()
        info = self.root.lstat()
        if (info.st_dev, info.st_ino) != self.root_identity:
            raise Refused("fixture root identity changed")
        shutil.rmtree(self.root)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    args = parser.parse_args()
    if sys.platform != "darwin" or not args.binary.is_absolute():
        raise Refused("Mac fixture and absolute binary required")
    info = args.binary.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
        raise Refused("fixture binary ownership")
    fixture = Fixture(args.binary)
    try:
        fixture.run()
    finally:
        fixture.close()


if __name__ == "__main__":
    try:
        main()
    except (Refused, OSError, ValueError, subprocess.TimeoutExpired) as error:
        # Only fixed fixture categories; never print child stdout/stderr,
        # command arguments, paths, passphrases, or traceback chains.
        print("FAIL", str(error) if isinstance(error, Refused) else "fixture operation refused", file=sys.stderr)
        sys.exit(1)
