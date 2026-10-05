#!/usr/bin/env python3
"""No-model native MCP probe. The disposable server cannot access network/files.

Run with the installed, authenticated Claude Code: python3 scripts/probe-claude-elicitation.py
This verifies native wire shapes, not the required billed chat-smoke gate.
"""
import json
import os
from pathlib import Path
import selectors
import shutil
import subprocess
import tempfile
import time


def main():
    binary = shutil.which("claude")
    if binary is None:
        raise RuntimeError("Claude Code is not installed")
    fixture = Path(__file__).resolve().parents[1] / "crates/chimaera-agent/tests/fixtures/elicitation.py"
    config = {"mcpServers": {"elicit": {"command": "python3", "args": [str(fixture)]}}}
    with tempfile.TemporaryDirectory(prefix="chimaera-mcp-wire-") as work:
        with open(Path(work) / "stderr", "w") as stderr:
            child = subprocess.Popen(
                [binary, "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--permission-prompt-tool", "stdio", "--strict-mcp-config", "--mcp-config", json.dumps(config), "--setting-sources", "user"],
                cwd=work, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr,
                env={key: value for key, value in os.environ.items() if key != "CLAUDECODE"},
            )
            selector = selectors.DefaultSelector()
            selector.register(child.stdout, selectors.EVENT_READ)
            buffer = b""
            serial = 0
            observed = []

            def send(frame):
                child.stdin.write(json.dumps(frame).encode() + b"\n")
                child.stdin.flush()

            def request(payload, action="cancel"):
                nonlocal serial, buffer
                serial += 1
                identity = str(serial)
                send({"type": "control_request", "request_id": identity, "request": payload})
                deadline = time.monotonic() + 60
                while time.monotonic() < deadline:
                    if b"\n" not in buffer:
                        if not selector.select(1):
                            continue
                        chunk = os.read(child.stdout.fileno(), 65536)
                        if not chunk:
                            raise RuntimeError("Claude exited before responding")
                        buffer += chunk
                        if len(buffer) > 2 * 1024 * 1024:
                            raise RuntimeError("Oversized protocol frame")
                    while b"\n" in buffer:
                        line, buffer = buffer.split(b"\n", 1)
                        frame = json.loads(line)
                        if frame.get("type") == "control_request" and frame.get("request", {}).get("subtype") == "elicitation":
                            ask = frame["request"]
                            observed.append((ask["mode"], action))
                            result = {"action": action}
                            if action == "accept" and ask["mode"] == "form":
                                result["content"] = {"label": "verified", "count": 0, "enabled": False, "tags": ["alpha"]}
                            send({"type": "control_response", "response": {"subtype": "success", "request_id": frame["request_id"], "response": result}})
                        if frame.get("type") == "control_response" and frame.get("response", {}).get("request_id") == identity:
                            response = frame["response"]
                            assert response["subtype"] == "success", response
                            return response.get("response", {})
                raise RuntimeError("Protocol request timed out")

            try:
                request({"subtype": "initialize", "hooks": {}})
                request({"subtype": "mcp_status"})
                for mode, action in [("form", "accept"), ("form", "decline"), ("url", "cancel")]:
                    result = request({"subtype": "mcp_call", "tool": "mcp__elicit__request_input", "arguments": {"mode": mode}}, action)
                    reply = json.loads(result["content"][0]["text"])
                    assert reply["action"] == action, reply
                    if action == "accept":
                        assert reply["content"] == {"label": "verified", "count": 0, "enabled": False, "tags": ["alpha"]}
                    else:
                        assert "content" not in reply, reply
                    print(f"Verified {mode} {action}")
                assert observed == [("form", "accept"), ("form", "decline"), ("url", "cancel")]
            finally:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
                selector.close()


if __name__ == "__main__":
    main()
