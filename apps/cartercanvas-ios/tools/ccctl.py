#!/usr/bin/env python3
"""Drive CarterCanvas over its control channel (docs/control-protocol.md).

The app listens when launched with CARTERCANVAS_CONTROL_PORT and CARTERCANVAS_CONTROL_TOKEN
(`simctl launch` takes them as SIMCTL_CHILD_…; the simulator shares the Mac's loopback).

    ccctl.py METHOD [JSON_PARAMS]            one request, prints the reply
    ccctl.py screenshot OUT.png              ui.screenshot saved to a file
    ccctl.py batch FILE.jsonl                one request per line, replies in order

Environment: CC_PORT (47600), CC_TOKEN (64 hex digits; the dev default below), CC_TIMEOUT
seconds (90).
"""
import base64
import json
import os
import socket
import sys

PORT = int(os.environ.get("CC_PORT", "47600"))
DEV_TOKEN = "cc" + "0" * 62
TOKEN = os.environ.get("CC_TOKEN", DEV_TOKEN)
TIMEOUT = float(os.environ.get("CC_TIMEOUT", "90"))


class Client:
    def __init__(self):
        self.sock = socket.create_connection(("127.0.0.1", PORT), timeout=TIMEOUT)
        self.file = self.sock.makefile("rwb")
        self.next_id = 0
        reply = self.call("auth", {"token": TOKEN})
        if not reply.get("ok"):
            raise SystemExit(f"authentication failed: {reply}")

    def call(self, method, params=None):
        self.next_id += 1
        req = {"id": self.next_id, "method": method, "params": params or {}}
        self.file.write((json.dumps(req) + "\n").encode())
        self.file.flush()
        line = self.file.readline()
        if not line:
            raise SystemExit("the app closed the connection")
        return json.loads(line)


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2
    c = Client()
    if argv[1] == "screenshot":
        out = argv[2] if len(argv) > 2 else "screenshot.png"
        r = c.call("ui.screenshot", {"focus": False})
        if not r.get("ok"):
            print(json.dumps(r, indent=2))
            return 1
        res = r["result"]
        data = res.get("base64") or res.get("png") or res.get("data")
        if data is None:
            print(json.dumps({k: v for k, v in res.items() if not isinstance(v, str) or len(v) < 200}, indent=2))
            return 1
        with open(out, "wb") as f:
            f.write(base64.b64decode(data))
        print(out)
        return 0
    if argv[1] == "batch":
        failed = 0
        with open(argv[2]) as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith("#"):
                    continue
                req = json.loads(line)
                r = c.call(req["method"], req.get("params"))
                failed += not r.get("ok")
                print(json.dumps(r)[:2000])
        return 1 if failed else 0
    params = json.loads(argv[2]) if len(argv) > 2 else {}
    r = c.call(argv[1], params)
    text = json.dumps(r, indent=2)
    print(text if len(text) < 20000 else text[:20000] + "\n…")
    return 0 if r.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
