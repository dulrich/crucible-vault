#!/usr/bin/env python3
"""usage: rpc.py <socket> <method> '<params-json>' — prints only aggregates (result size, latency)."""
import json, os, socket, sys, time
if any(a in ("-h", "--help") for a in sys.argv[1:]):
    print(__doc__.strip())
    sys.exit(0)
sys.path.insert(0, os.path.dirname(os.path.realpath(__file__)))
from build_id import handle_build_flags

_rc = handle_build_flags(sys.argv[1:], os.path.realpath(__file__), name="rpc.py", semver=None)
if _rc is not None:
    sys.exit(_rc)
sock, method, params = sys.argv[1], sys.argv[2], json.loads(sys.argv[3])
s = socket.socket(socket.AF_UNIX); s.connect(sock)
def call(m, prm, i):
    s.sendall((json.dumps({"jsonrpc": "2.0", "id": i, "method": m, "params": prm}) + "\n").encode())
    b = b""
    while not b.endswith(b"\n"):
        c = s.recv(1 << 20)
        if not c: break
        b += c
    return b
call("hello", {"protocol_version": 1, "client": "mem-spike"}, 0)
t = time.time()
buf = call(method, params, 1)
r = json.loads(buf)
res = r.get("result")
n = len(res.get("hits", [])) if isinstance(res, dict) and "hits" in res else None
print(json.dumps({"method": method, "ms": round((time.time()-t)*1000), "bytes": len(buf), "hits": n, "error": (r.get("error") or {}).get("message")}))
