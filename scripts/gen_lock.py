#!/usr/bin/env python3
"""Emit a large, syntactically valid package-lock.json for the FPS screenshot fixture.

The FPS shot's whole point is scrolling a huge file smoothly, so we just need a lot of
lines of realistic-looking JSON. Default ~37k lines (matches the README caption). Output
path is argv[1]; line target is argv[2] (optional)."""
import json
import sys

out = sys.argv[1]
target_lines = int(sys.argv[2]) if len(sys.argv) > 2 else 37000

# Each package entry is ~6 lines when pretty-printed at indent=2.
n = max(1, target_lines // 6)
packages = {"": {"name": "kyde-fps-fixture", "version": "1.0.0", "license": "MIT"}}
for i in range(n):
    name = f"node_modules/@scope/pkg-{i:05d}"
    packages[name] = {
        "version": f"{i % 9}.{i % 20}.{i % 50}",
        "resolved": f"https://registry.npmjs.org/@scope/pkg-{i:05d}/-/pkg-{i:05d}.tgz",
        "integrity": f"sha512-{(str(i) * 12)[:80]}==",
        "dev": i % 2 == 0,
    }

doc = {
    "name": "kyde-fps-fixture",
    "version": "1.0.0",
    "lockfileVersion": 3,
    "requires": True,
    "packages": packages,
}
with open(out, "w") as f:
    json.dump(doc, f, indent=2)
    f.write("\n")

with open(out) as f:
    print(sum(1 for _ in f))
