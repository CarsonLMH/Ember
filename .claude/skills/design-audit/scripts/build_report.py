#!/usr/bin/env python3
"""Inline evidence crops into a design-audit report.

    build_report.py <template.html> <out.html> [ev-dir]

Replaces every {{IMG:name}} with a data URI for <ev-dir>/name.jpg (or .png).
ev-dir defaults to ./ev next to the template. Fails loudly on a missing image
so a report never ships with a broken placeholder.
"""
import base64, pathlib, re, sys

tpl_path = pathlib.Path(sys.argv[1])
out_path = pathlib.Path(sys.argv[2])
ev = pathlib.Path(sys.argv[3]) if len(sys.argv) > 3 else tpl_path.parent / "ev"
tpl = tpl_path.read_text()

def uri(name: str) -> str:
    for ext in ("jpg", "jpeg", "png"):
        p = ev / f"{name}.{ext}"
        if p.exists():
            data = p.read_bytes()
            mime = "image/jpeg" if data[:2] == b"\xff\xd8" else "image/png"
            return f"data:{mime};base64,{base64.b64encode(data).decode()}"
    sys.exit(f"missing evidence image: {ev}/{name}.(jpg|png)")

out = re.sub(r"\{\{IMG:([A-Za-z0-9_-]+)\}\}", lambda m: uri(m.group(1)), tpl)
leftover = re.findall(r"\{\{[A-Z_]+[^}]*\}\}", out)
if leftover:
    sys.exit(f"unfilled placeholders: {sorted(set(leftover))[:8]}")
out_path.write_text(out)
print(f"{out_path} ({len(out)//1024} KB)")
