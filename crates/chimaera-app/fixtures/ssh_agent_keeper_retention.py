#!/usr/bin/env python3
"""C1: actual Link jobs/forwards across native exit, synthetic scheduler/account.

Requires the explicit fixture binary observer/native dispatch and reviewed
private retention bootstrap/control. Source wiring is not a runtime claim.
Run only with synthetic identities and immutable task-owned Mac/Linux artifacts.
"""
import argparse
import base64
import json
import os
import pathlib
import select
import socket
import signal
import stat
import subprocess
import sys
import time

from ssh_agent_keeper import (Keeper, Gateway, CountingProxy, LINUX_BINARY,
                              LINUX_WRAPPER, Refused)
from ssh_agent_loader import capture, stop_owned


def causal_diagnostic(phase, error):
    # Closed phase/type/source-line observations only; no exception arguments,
    # child bytes, identities, paths or traceback source text are published.
    phases = ("bootstrap", "ready", "host-trust", "proxy-config", "native",
              "helper-receipt", "native-answer", "start-jobs", "held",
              "native-settle", "proxy-retire", "running", "malformed",
              "stop-attached", "uncertain", "restore", "attached-ended",
              "jobs-ended", "quiet", "finished", "final-check",
              "case-cleanup", "outer-run", "outer-cleanup")
    kind = ("refused" if isinstance(error, Refused)
            else "timeout" if isinstance(error, (TimeoutError, subprocess.TimeoutExpired))
            else "os" if isinstance(error, OSError)
            else "value" if isinstance(error, ValueError) else "other")
    here = os.path.dirname(os.path.abspath(__file__))
    allowed = {os.path.join(here, filename): module for filename, module in (
        ("ssh_agent_keeper_retention.py", "retention"),
        ("ssh_agent_keeper.py", "keeper"), ("ssh_agent_loader.py", "loader"))}
    frames, frame = [], error.__traceback__
    for _ in range(32):
        if frame is None:
            break
        module = allowed.get(os.path.abspath(frame.tb_frame.f_code.co_filename))
        if module is not None and 0 < frame.tb_lineno <= 1000000 and len(frames) < 8:
            frames.append({"module": module, "line": frame.tb_lineno})
        frame = frame.tb_next
    fact = {"phase": phase if phase in phases else "other", "kind": kind, "frames": frames}
    try:
        print("DIAGNOSTIC C1 cause", json.dumps(fact, separators=(",", ":")), flush=True)
    except (OSError, ValueError):
        pass  # A closed diagnostic sink cannot replace the original exception.
    return fact


def held_refusal(value):
    # A diagnostic is never positive lifetime proof. Names are emitted solely by
    # the fixed private fixture; unexpected data is refused without rendering.
    stages = (
        'start', 'scheduler-read', 'scheduler-identity', 'runtime',
        'journal', 'snapshot', 'hold-ack', 'prior-capture',
        'held-account', 'job-phases', 'attached', 'forward-count',
        'scheduler-running', 'master-resolve', 'master-count', 'master-presence',
        'auth-runtime', 'auth-stop', 'auth-paths', 'capture-task',
        'auth-active', 'cgroup', 'forward-shape', 'forward-sockets',
        'forward-holder', 'pty-capture', 'pty-start', 'pty-tty',
        'hops-scan', 'hop-capture', 'hop-presence', 'pty-live',
        'captured', 'invalid-stage',
    )
    if (type(value) is not dict or set(value) != {"type", "stage", "timeout"}
            or value["type"] != "c1_held_refused" or type(value["stage"]) is not str
            or value["stage"] not in stages or type(value["timeout"]) is not bool):
        return None
    return {"stage": value["stage"], "timeout": value["timeout"]}


def stop_refusal(output):
    # Only the exact fixed native failure marker plus fixed dispatch failure can
    # yield a diagnostic. Arbitrary child output remains unrendered refusal.
    stages = ("arguments", "client-build", "me", "capabilities", "stop-reply",
              "snapshot", "pending-projection", "batch-state", "batch-health", "route-denial")
    if type(output) is not bytes or len(output) > 128:
        return None
    for stage in stages:
        if output == b"C1_STOP_REFUSED " + stage.encode("ascii") + b"\nC1_LINK_FAILED\n":
            return {"stage": stage}
    return None


def stop_diagnostic(fact):
    stages = ("arguments", "client-build", "me", "capabilities", "stop-reply",
              "snapshot", "pending-projection", "batch-state", "batch-health", "route-denial",
              "capture-unsettled", "unclassified-output")
    if (type(fact) is not dict or set(fact) != {"stage"}
            or type(fact["stage"]) is not str or fact["stage"] not in stages):
        return
    try:
        print("DIAGNOSTIC C1 stop refusal", json.dumps(fact, separators=(",", ":")), flush=True)
    except (OSError, ValueError):
        pass  # Diagnostic failure cannot replace the original refusal/cleanup.


class RetentionKeeper(Keeper):
    def __init__(self, *args):
        original = time.monotonic() + 300
        super().__init__(*args)
        # Parent constructors keep their legacy bounds; C1 restores the exact
        # pre-construction original envelope, never now+300 after effects.
        self.deadline = original

    def control(self, process, command, expected, seconds=5):
        # Neither selectors nor scheduler authority arrive from this input.
        if command not in (b"HELD\n", b"RETAINED\n", b"MALFORMED\n", b"UNCERTAIN\n",
                           b"RESTORE\n", b"ATTACHED_ENDED\n", b"JOBS_ENDED\n", b"FINISHED\n"):
            raise Refused("c1 control invalid")
        if process.poll() is not None or os.write(process.stdin.fileno(), command) != len(command):
            raise Refused("c1 control unavailable")
        value = self.record(process, seconds)
        if command == b"HELD\n":
            refusal = held_refusal(value)
            if refusal is not None:
                try:
                    print("DIAGNOSTIC C1 held refusal", json.dumps(refusal, separators=(",", ":")), flush=True)
                except (OSError, ValueError):
                    pass
                raise Refused("c1 held proof refused")
        if (type(value) is not dict or set(value) != set(expected)
                or any(type(value[key]) is not type(wanted) or value[key] != wanted
                       for key, wanted in expected.items())):
            raise Refused("c1 captured lifetime receipt")

    @staticmethod
    def connections(gateway):
        with gateway.lock:
            return gateway.total

    def observer(self, gateway, phase):
        markers = {"start": b"C1_LINK_STARTED\n", "running": b"C1_LINK_RUNNING\n",
                   "stop-attached": b"C1_LINK_UNCERTAIN\n",
                   "attached-ended": b"C1_LINK_ATTACHED_ENDED\n", "jobs-ended": b"C1_LINK_JOBS_ENDED\n",
                   "finished": b"C1_LINK_FINISHED\n"}
        if phase not in markers:
            raise Refused("c1 observer invalid")
        budget = {"start": 30, "running": 8, "stop-attached": 16, "attached-ended": 6, "jobs-ended": 4, "finished": 4}[phase]
        before = self.connections(gateway)
        remaining_ms = int(max(0, self.deadline - 15 - time.monotonic()) * 1000)
        if not 1 <= remaining_ms <= 300000:
            raise Refused("c1 original observer budget")
        process = self.spawn([str(self.binary), "--keeper-retention-observer", gateway.origin, phase,
                              str(remaining_ms)], self.env)
        try:
            try:
                output = capture(process, self.end(21 if phase == "start" else 11), 8192)
            except BaseException:
                if phase == "stop-attached":
                    stop_diagnostic({"stage": "capture-unsettled"})
                raise
            if phase == "jobs-ended" and process.returncode == 0 and output == b"C1_LINK_BATCH_PENDING\n":
                return False
            if process.returncode or output != markers[phase]:
                if phase == "stop-attached":
                    stop_diagnostic(stop_refusal(output) or {"stage": "unclassified-output"})
                raise Refused("c1 authenticated Link observation")
            return True
        finally:
            stop_owned(process)
            if self.connections(gateway) - before > budget:
                raise Refused("c1 observer bridge budget")

    def run(self):
        for path, digest in ((LINUX_BINARY, self.digest), (LINUX_WRAPPER, self.wrapper_digest)):
            check = self.management("/usr/bin/sha256sum " + path)
            try:
                if capture(check, self.end(5), 256).split() != [digest.encode(), path.encode()] or check.returncode:
                    raise Refused("c1 immutable Linux receipt")
            finally:
                stop_owned(check)
        encrypted, public = self.key("retention-selected")
        duplicate = self.root / "retention-duplicate"
        duplicate.write_bytes(encrypted.read_bytes())
        os.chmod(duplicate, 0o600)
        if self.collect(["/usr/bin/ssh-keygen", "-q", "-p", "-P", "fixture-only-passphrase", "-N", "", "-f", str(duplicate)])[0]:
            raise Refused("c1 duplicate preparation")
        upstream = self.root / "retention-external"
        agent = self.spawn(["/usr/bin/ssh-agent", "-D", "-P", "", "-a", str(upstream)], output=False)
        until = self.end(2)
        while not upstream.exists() and time.monotonic() < until:
            time.sleep(.02)
        external_env = dict(self.env, SSH_AUTH_SOCK=str(upstream), SSH_ASKPASS_REQUIRE="never")
        if self.collect(["/usr/bin/ssh-add", str(duplicate)], external_env)[0]:
            raise Refused("c1 external seed")
        before = self.identities(external_env)
        gateway = Gateway(self)
        control = self.management("/usr/bin/env -i PATH=/usr/bin:/bin:/usr/sbin HOME=/tmp /usr/bin/python3 -I -S " + LINUX_WRAPPER + " --retention",
                                  input_pipe=True, control=True)
        app = proxy = None
        receipts = []
        phase = "bootstrap"
        original_error = None
        try:
            body = json.dumps({"advertised_port": gateway.server_address[1], "retention": True,
                               "public_key": "ssh-ed25519 " + public.decode()}, separators=(",", ":")).encode() + b"\n"
            if os.write(control.stdin.fileno(), body) != len(body):
                raise Refused("c1 bootstrap handoff")
            phase = "ready"
            ready = self.record(control, 20)
            if (ready.get("type") != "ready" or ready.get("retention") is not True
                    or type(ready.get("pid")) is not int or ready["pid"] != ready.get("pgid")
                    or type(ready.get("port")) is not int or not 1 <= ready["port"] <= 65535
                    or not isinstance(ready.get("ssh_ports"), list) or len(ready["ssh_ports"]) != 2
                    or not isinstance(ready.get("host_keys"), list) or len(ready["host_keys"]) != 2):
                raise Refused("c1 startup identity")
            phase = "host-trust"
            gateway.port = ready["port"]
            known, rows, host_keys = self.root / "retention-known", [], []
            for port, host in zip(ready["ssh_ports"], ready["host_keys"]):
                if type(port) is not int or not 1 <= port <= 65535 or type(host) is not str or len(host) > 16384:
                    raise Refused("c1 host key receipt")
                parts = host.split()
                if len(parts) != 3 or parts[0] != "ssh-ed25519":
                    raise Refused("c1 host key type")
                key = base64.b64decode(parts[1], validate=True)
                if not 1 <= len(key) <= 8192:
                    raise Refused("c1 host key bound")
                host_keys.append(key)
                rows.append("[127.0.0.1]:" + str(port) + " " + " ".join(parts[:2]) + "\n")
            known.write_text("".join(rows))
            os.chmod(known, 0o600)
            phase = "proxy-config"
            proxy = CountingProxy(self, self.root / "retention-proxy", upstream, agent, "accept", host_keys)
            proxy.start()
            gateway.start()
            config = self.root / "retention-config"
            config.write_text("Host keeper-route-fixture\n HostName 127.0.0.1\n Port " + str(ready["ssh_ports"][1]) +
                "\n ProxyJump keeper-route-hop\n IdentityAgent " + str(proxy.path) + "\n IdentitiesOnly no\n"
                "Host keeper-route-hop\n HostName 127.0.0.1\n Port " + str(ready["ssh_ports"][0]) + "\n IdentityAgent none\n IdentitiesOnly yes\n"
                "Host *\n User root\n IdentityFile " + str(encrypted) + "\n UserKnownHostsFile " + str(known) +
                "\n GlobalKnownHostsFile none\n PubkeyAuthentication yes\n PasswordAuthentication no\n KbdInteractiveAuthentication no\n"
                " GSSAPIAuthentication no\n HostbasedAuthentication no\n PreferredAuthentications publickey\n"
                " HostKeyAlgorithms ssh-ed25519\n PubkeyAcceptedAlgorithms ssh-ed25519\n CASignatureAlgorithms ssh-ed25519\n ControlMaster no\n ControlPath none\n")
            os.chmod(config, 0o600)
            phase = "native"
            app = self.spawn([str(self.binary), "--keeper-route-fixture", str(config), gateway.origin, "retention"], self.env, input_pipe=True)
            output, receipts, answered, held = bytearray(), [], 0, False
            end = self.end(31)
            while True:
                if time.monotonic() >= end:
                    raise Refused("c1 original native deadline")
                if not select.select([app.stdout], [], [], .025)[0]:
                    continue
                chunk = os.read(app.stdout.fileno(), 4096)
                if not chunk:
                    break
                if len(output) + len(chunk) > 8192:
                    raise Refused("c1 native output bound")
                output.extend(chunk)
                prompts = output.count(b"KEEPER_PROMPT\n")
                if prompts > 2:
                    raise Refused("c1 native prompt count")
                while answered < prompts:
                    phase = "helper-receipt"
                    receipts.append(self.helper_receipt(app))
                    phase = "native-answer"
                    if os.write(app.stdin.fileno(), b"CONTINUE\n") != 9:
                        raise Refused("c1 original prompt handoff")
                    answered += 1
                if b"KEEPER_CONNECTED\n" in output and not held:
                    if answered != 2:
                        raise Refused("c1 native selection receipt")
                    before_start = self.connections(gateway)
                    phase = "start-jobs"
                    self.observer(gateway, "start")
                    start_connections = self.connections(gateway) - before_start
                    phase = "held"
                    self.control(control, b"HELD\n", {"type": "c1_held", "jobs": 2, "submissions": 2,
                        "attached_alive": True, "forwards": 2, "master_same": True, "account_held": True})
                    if os.write(app.stdin.fileno(), b"HELD\n") != 5:
                        raise Refused("c1 positive held handoff")
                    app.stdin.close()
                    held = True
            phase = "native-settle"
            app.wait(timeout=max(.01, end - time.monotonic()))
            counts = proxy.leg_counts()
            if app.returncode or b"KEEPER_AUTHENTICATED\n" not in output or not held or counts != ((0, 0), (1, 1)):
                raise Refused("c1 original native settled")
            for receipt in receipts:
                self.cleanup_receipt(receipt)
            if self.connections(gateway) - start_connections > 16:
                raise Refused("c1 native bridge budget")
            # Positive retirement removes the original counting/signing endpoint.
            # No later observer has an agent/key selector or SSH Connect path.
            phase = "proxy-retire"
            proxy.close()
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as probe:
                probe.settimeout(1)
                try:
                    probe.connect(str(proxy.path))
                except FileNotFoundError:
                    pass
                else:
                    raise Refused("c1 retired agent endpoint present")
            # Original process is reaped and its helper directories are absent.
            # Reopen actual authenticated route views, without any new Connect.
            retained = {"type": "c1_retained", "jobs": 2, "submissions": 2, "attached_alive": True,
                        "forwards": 2, "master_same": True, "account_held": True, "auth_closed": True}
            observation = time.monotonic()
            for round_ in range(3):
                if round_:
                    time.sleep(2.5)
                phase = "running"
                self.observer(gateway, "running")
                self.control(control, b"RETAINED\n", retained)
            if time.monotonic() - observation < 5:
                raise Refused("c1 retained observation duration")
            phase = "malformed"
            self.control(control, b"MALFORMED\n", {"type": "c1_malformed", "selected": True})
            phase = "stop-attached"
            self.observer(gateway, "stop-attached")
            phase = "uncertain"
            self.control(control, b"UNCERTAIN\n", {"type": "c1_uncertain", "attached_nonterminal": True,
                "attached_alive": True, "attached_forward_same": True, "account_held": True, "submissions": 2})
            phase = "restore"
            self.control(control, b"RESTORE\n", {"type": "c1_restored", "selected": True})
            phase = "attached-ended"
            self.observer(gateway, "attached-ended")
            self.control(control, b"ATTACHED_ENDED\n", {"type": "c1_attached_ended", "attached_pty_absent": True,
                "attached_forward_absent": True, "batch_forward_same": True, "master_same": True,
                "account_held": True, "submissions": 2})
            # The scheduler's original 60-second batch deadline advances itself.
            # No expiry-control command or batch StopJob can fabricate TIMEOUT.
            phase = "jobs-ended"
            jobs_ended = False
            for round_ in range(8):
                if time.monotonic() >= self.deadline - 15:
                    break
                if self.observer(gateway, "jobs-ended"):
                    jobs_ended = True
                    break
                if control.poll() is not None or gateway.failed or proxy.failed:
                    raise Refused("c1 expiry observer failed")
                if round_ < 7:
                    if time.monotonic() + 8 >= self.deadline - 15:
                        break
                    time.sleep(8)
            if not jobs_ended:
                raise Refused("c1 independent batch expiry")
            # Terminal resources settle independently of normal login grace.
            last_refresh_done = time.monotonic()
            self.control(control, b"JOBS_ENDED\n", {"type": "c1_jobs_ended", "terminal": 2,
                "attached_pty_absent": True, "forwards_absent": True, "submissions": 2})
            #120s seed + up to60s normal background poll +5s ACK margin.
            # No refresh/auth/target-health/control requests occur in this quiet
            # interval. A passive view afterward cannot renew that original seed.
            phase = "quiet"
            quiet_until = last_refresh_done + 185
            if quiet_until + 10 >= self.deadline - 15:
                raise Refused("c1 original quiet observation budget")
            quiet_connections = self.connections(gateway)
            while time.monotonic() < quiet_until:
                if control.poll() is not None or gateway.failed or proxy.failed:
                    raise Refused("c1 quiet owner failed")
                time.sleep(max(0, min(.25, quiet_until - time.monotonic())))
            if self.connections(gateway) != quiet_connections:
                raise Refused("c1 quiet observation made a request")
            phase = "finished"
            self.observer(gateway, "finished")
            self.control(control, b"FINISHED\n", {"type": "c1_finished", "terminal": 2,
                "attached_pty_absent": True, "forwards_absent": True, "masters_absent": True,
                "hop_helpers_absent": True, "account_idle": True, "login_opt_in": False, "submissions": 2})
            phase = "final-check"
            if (not proxy.closed or proxy.path.exists() or gateway.failed or proxy.failed
                    or self.connections(gateway) > 128 or self.identities(external_env) != before):
                raise Refused("c1 no successor endpoint and bridge budget")
        except BaseException as error:
            original_error = error
            causal_diagnostic(phase, error)
            raise
        finally:
            try:
                if app is not None:
                    # Existing shared cleanup independently attempts app, proxy,
                    # gateway and exact two-directory inner/external receipts.
                    self.cleanup_case(app, gateway, control, proxy)
                else:
                    # A bootstrap failure has no native app owner, but every
                    # created bridge/control still owes its positive cleanup.
                    failed = False
                    for operation in ([proxy.close] if proxy is not None else []) + [
                            gateway.close, lambda: self.cleanup_control(control, expected_directories=2)]:
                        try:
                            operation()
                        except Exception as error:
                            failed = True
                            causal_diagnostic("case-cleanup", error)
                    self.cleanup_failure(failed, original_error is not None)
            except Exception as error:
                causal_diagnostic("case-cleanup", error)
                if original_error is None:
                    raise
        print("PASS retention C1 actual Link job/forward across native exit; no original agent endpoint; explicit attached stop, independent batch timeout and passive post-grace retirement; synthetic account/scheduler; positive inner/external cleanup", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--podman", type=pathlib.Path, default=pathlib.Path("/opt/homebrew/bin/podman"))
    parser.add_argument("--linux-sha256", required=True)
    parser.add_argument("--wrapper-sha256", required=True)
    args = parser.parse_args()
    args.podman = args.podman.resolve(strict=True)
    if sys.platform != "darwin" or any(len(value) != 64 or any(c not in "0123456789abcdef" for c in value)
                                      for value in (args.linux_sha256, args.wrapper_sha256)):
        raise Refused("c1 immutable Mac/Linux provenance required")
    for path in (args.binary, args.podman):
        info = path.lstat()
        if not path.is_absolute() or not stat.S_ISREG(info.st_mode) or info.st_uid not in (0, os.getuid()) or info.st_mode & 0o022:
            raise Refused("c1 fixed executable ownership")
    def interrupted(*_):
        raise Refused("c1 interrupted or expired")
    for name in (signal.SIGALRM, signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(name, interrupted)
    signals = {signal.SIGALRM, signal.SIGTERM, signal.SIGINT, signal.SIGHUP}
    previous = signal.pthread_sigmask(signal.SIG_BLOCK, signals)
    try:
        fixture = RetentionKeeper(args.binary, args.podman, args.linux_sha256, args.wrapper_sha256)
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous)
    signal.setitimer(signal.ITIMER_REAL, max(.001, fixture.deadline - time.monotonic()))
    original_error = None
    try:
        fixture.run()
    except BaseException as error:
        original_error = error
        causal_diagnostic("outer-run", error)
        raise
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        for name in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            signal.signal(name, signal.SIG_IGN)
        try:
            fixture.close()
        except Exception as error:
            causal_diagnostic("outer-cleanup", error)
            if original_error is None:
                raise


if __name__ == "__main__":
    try:
        main()
    except (Refused, OSError, ValueError, TimeoutError):
        print("REFUSED C1 retention fixture", file=sys.stderr)
        raise SystemExit(2)
