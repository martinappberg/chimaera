"""Disposable stdio MCP server for live elicitation probes; no network or credentials."""
import json
import sys


def emit(value):
    print(json.dumps({"jsonrpc": "2.0", **value}), flush=True)


pending = {}
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    identity = request.get("id")
    if method == "initialize":
        emit({"id": identity, "result": {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}}, "serverInfo": {"name": "elicitation-fixture", "version": "1"}}})
    elif method == "tools/list":
        emit({"id": identity, "result": {"tools": [{"name": "request_input", "description": "Exercise a user form or browser request.", "inputSchema": {"type": "object", "properties": {"mode": {"type": "string", "enum": ["form", "url"]}}}}]}})
    elif method == "tools/call":
        mode = request["params"].get("arguments", {}).get("mode", "form")
        ask = f"ask-{identity}"
        pending[ask] = identity
        params = {"mode": mode, "message": "Configure this fixture request"}
        if mode == "url":
            params.update({"url": "https://example.com/chimaera-mcp-fixture", "elicitationId": ask})
        else:
            params["requestedSchema"] = {"type": "object", "required": ["label", "count", "enabled"], "properties": {
                "label": {"type": "string", "title": "Label", "minLength": 1, "maxLength": 32},
                "count": {"type": "integer", "title": "Count", "minimum": 0, "maximum": 10, "default": 0},
                "enabled": {"type": "boolean", "title": "Enabled", "default": False},
                "tags": {"type": "array", "title": "Tags", "items": {"type": "string", "enum": ["alpha", "beta"]}},
            }}
        emit({"id": ask, "method": "elicitation/create", "params": params})
    elif identity in pending:
        emit({"id": pending.pop(identity), "result": {"content": [{"type": "text", "text": json.dumps(request.get("result", request.get("error")))}]}})
    elif identity is not None:
        emit({"id": identity, "result": {}})
