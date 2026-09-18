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
        if "--enforce-sandbox" in sys.argv and (msg["params"].get("sandbox") != "workspace-write" or msg["params"].get("approvalPolicy") != "on-request"):
            print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32600, "message": "thread policy mismatch"}}), flush=True)
            continue
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"thread": {"id": "thread-one"}}}), flush=True)
    elif method == "thread/resume":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"thread": {"id": msg["params"]["threadId"]}}}), flush=True)
    elif method == "turn/start":
        policy = msg["params"].get("sandboxPolicy", {})
        user_input = msg["params"].get("input", [])
        if not user_input or any(not isinstance(item, dict) or item.get("type") not in ("text", "image") for item in user_input):
            print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32602, "message": "invalid App Server user input"}}), flush=True)
            continue
        if "--enforce-sandbox" in sys.argv and (policy.get("type") != "workspaceWrite" or policy.get("writableRoots") != ["/tmp"] or policy.get("networkAccess") is not False or msg["params"].get("approvalPolicy") != "on-request"):
            print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32600, "message": "turn policy mismatch"}}), flush=True)
            continue
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"turn": {"id": "turn-one"}}}), flush=True)
        if "--request-approval" in sys.argv:
            print(json.dumps({"jsonrpc":"2.0","id":"approval-one","method":"item/commandExecution/requestApproval","params":{"threadId":"thread-one","turnId":"turn-one","itemId":"item-one","command":"cat report.txt","availableDecisions":["accept","decline"]}}), flush=True)
    elif method == "thread/read":
        items = [{"type": "agentMessage", "phase": "final", "text": "DONE"}]
        if "--generated-image" in sys.argv:
            items.append({"type":"imageGeneration","id":"image-one","status":"completed","result":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jGZkAAAAASUVORK5CYII="})
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"thread": {"id": msg["params"]["threadId"], "turns": [{"id": "turn-one", "status": "completed", "itemsView": "full", "items": items}]}}}), flush=True)
    elif method == "model/list":
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{"data":[{"id":"gpt-5.6-luna","displayName":"GPT 5.6 Luna"}],"nextCursor":None}}),flush=True)
    elif method == "turn/steer":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"turnId": msg["params"]["expectedTurnId"]}}), flush=True)
    elif method == "turn/interrupt":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
    elif method is None and msg.get("id") == "approval-one":
        print(json.dumps({"jsonrpc":"2.0","method":"serverRequest/resolved","params":{"threadId":"thread-one","requestId":"approval-one"}}), flush=True)
