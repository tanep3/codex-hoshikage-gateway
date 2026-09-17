#!/usr/bin/env python3
"""Deterministic stdio peer for Gateway transport acceptance tests."""
import json
import sys
import time

for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        if "--hang-init" in sys.argv:
            time.sleep(60)
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"userAgent": "mock"}}), flush=True)
    elif method == "test/null":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": None}), flush=True)
    elif method == "test/collision":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "method": "item/commandExecution/requestApproval", "params": {"command": "true"}}), flush=True)
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"ok": True}}), flush=True)
    elif method == "test/string-request":
        print(json.dumps({"jsonrpc": "2.0", "id": "approval-X", "method": "item/commandExecution/requestApproval", "params": {"command": "true"}}), flush=True)
        reply = json.loads(sys.stdin.readline())
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"replyId": reply["id"], "decision": reply.get("result")}}), flush=True)
    elif method == "test/duplicate-request":
        print('{"jsonrpc":"2.0","id":"bad","method":"item/commandExecution/requestApproval","params":{"command":"safe","command":"danger"}}', flush=True)
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"ok": True}}), flush=True)
    elif method == "test/hang":
        time.sleep(60)
    elif method == "test/exit":
        sys.exit(0)
    elif method == "test/echo":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": msg["params"]}), flush=True)
    elif method == "thread/start":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"thread": {"id": "thread-one"}}}), flush=True)
    elif method == "thread/resume":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"thread": {"id": msg["params"]["threadId"]}}}), flush=True)
    elif method == "turn/start":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"turn": {"id": "turn-one"}}}), flush=True)
    elif method == "thread/read":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"thread": {"id": msg["params"]["threadId"], "turns": [{"id": "turn-one", "status": "completed", "items": [{"type": "agentMessage", "phase": "final", "text": "DONE"}]}]}}}), flush=True)
    elif method == "turn/steer":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"turnId": msg["params"]["expectedTurnId"]}}), flush=True)
    elif method == "turn/interrupt":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
