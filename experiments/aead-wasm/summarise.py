#!/usr/bin/env python3
"""Tabulate experiments/aead-wasm results: ns per message, and ChaCha's
speed-up over AES-256-GCM, per build, op and size.

    python3 summarise.py results.jsonl
"""
import json
import sys
from collections import defaultdict

rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
ns = defaultdict(dict)
for r in rows:
    ns[(r["build"], r["op"], r["size"])][r["cipher"]] = r["ns"]

builds = list(dict.fromkeys(r["build"] for r in rows))
for op in ["open", "seal", "key"]:
    print(f"\n{op}: µs per message (fresh key each time); ChaCha speed-up over AES")
    print(f"{'build':<22}{'size':>6}{'AES-GCM':>10}{'ChaCha':>10}{'XChaCha':>10}{'speed-up':>10}")
    for b in builds:
        for size in [16, 64, 256, 2048]:
            c = ns.get((b, op, size))
            if not c:
                continue
            aes, cha, x = c["aes-256-gcm"] / 1000, c["chacha20-poly1305"] / 1000, c["xchacha20-poly1305"] / 1000
            print(f"{b:<22}{size:>6}{aes:>10.2f}{cha:>10.2f}{x:>10.2f}{aes / cha:>9.1f}x")
