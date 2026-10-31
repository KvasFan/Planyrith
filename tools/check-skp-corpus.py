#!/usr/bin/env python3
"""SKP exchange certification over a local corpus (docs/dev/skp-fidelity.md).

    tools/check-skp-corpus.py CORPUS_DIR [--oracle DIR] [--jobs N]

Every 2013-or-later model below CORPUS_DIR goes through the acceptance chain of
docs/13 0.11 with the optimized CLI and converter: open, native save/reopen,
byte-exact unchanged restoration, edit (add a line, delete a face, rename a
material, tag, scene and component), regenerate in both writers, reopen. Counts
and the new names must survive; failures are listed and the exit code is 1.

--oracle DIR holds `<year>/<name>.skp` next to `2017/<name>.dae` exports written
by the original application. Their world-space surfaces are compared with the
import and both regenerated files (bounds, area, vertices). Placed images are
surfaces in those exports but not in the OBJ used here, and each side of a
face is counted once. Texture coordinates are compared per side at every corner
the native document places explicitly. That export bakes projectively pinned
textures into per-face images, so such faces disagree by construction.

Models are copied to temporary folders; the corpus is only read. Build first:
cargo build --release -p planyrith-cli --features render
cargo build --release -p planyrith-skp
"""
import argparse
import concurrent.futures as cf
import filecmp
import json
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CLI = os.path.join(ROOT, "target/release/planyrith")
ENV = dict(os.environ, PLANYRITH_SKP_ADAPTER=os.path.join(ROOT, "target/release/planyrith-skp"))
FAR = ("9000000mm,9000000mm,9000000mm", "9001000mm,9000000mm,9000000mm")
COUNTS = ("faces", "edges", "texts", "dimensions", "guides", "section_planes", "images", "materials", "scenes")
HEADER = re.compile(rb"\{\x00?(\d\x00?\d?\x00?(?:\.\x00?\d\x00?)+(?:\d\x00?)*)\}")
NS = "{http://www.collada.org/2005/11/COLLADASchema}"


class HoledRing(list):
    """A polygon's outer ring; `holes` carries its inner rings."""

    def __init__(self, outer, holes):
        super().__init__(outer)
        self.holes = holes


def mat_mul(a, b):
    return [[sum(a[r][k] * b[k][c] for k in range(4)) for c in range(4)] for r in range(4)]


def apply(m, p):
    return tuple(m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3] for r in range(3))


def area(ring):
    n = [0.0, 0.0, 0.0]
    for i, a in enumerate(ring):
        b = ring[(i + 1) % len(ring)]
        n[0] += (a[1] - b[1]) * (a[2] + b[2])
        n[1] += (a[2] - b[2]) * (a[0] + b[0])
        n[2] += (a[0] - b[0]) * (a[1] + b[1])
    return 0.5 * math.sqrt(sum(x * x for x in n))


def dae_triangles(path):
    root = ET.parse(path).getroot()
    unit = root.find(f"{NS}asset/{NS}unit")
    scale = float(unit.get("meter")) * 1000 if unit is not None else 25.4
    geoms = {}
    for g in root.iter(f"{NS}geometry"):
        mesh = g.find(f"{NS}mesh")
        if mesh is None:
            continue
        sources = {}
        for s in mesh.findall(f"{NS}source"):
            arr = s.find(f"{NS}float_array")
            sources[s.get("id")] = [float(x) for x in arr.text.split()] if arr is not None and arr.text else []
        verts = {}
        for v in mesh.findall(f"{NS}vertices"):
            for i in v.findall(f"{NS}input"):
                if i.get("semantic") == "POSITION":
                    verts[v.get("id")] = i.get("source")[1:]
        tris = []
        for t in mesh.findall(f"{NS}triangles"):
            inputs = t.findall(f"{NS}input")
            stride = max(int(i.get("offset")) for i in inputs) + 1
            off = pos = None
            for i in inputs:
                if i.get("semantic") == "VERTEX":
                    off = int(i.get("offset"))
                    pos = sources[verts[i.get("source")[1:]]]
            ptxt = t.find(f"{NS}p")
            if ptxt is None or not ptxt.text:
                continue
            idx = [int(x) for x in ptxt.text.split()]
            for k in range(0, len(idx), stride * 3):
                tri = []
                for c in range(3):
                    n = idx[k + c * stride + off]
                    tri.append((pos[3 * n], pos[3 * n + 1], pos[3 * n + 2]))
                tris.append(tri)
        for t in mesh.findall(f"{NS}polylist"):
            inputs = t.findall(f"{NS}input")
            stride = max(int(i.get("offset")) for i in inputs) + 1
            for i in inputs:
                if i.get("semantic") == "VERTEX":
                    off = int(i.get("offset"))
                    pos = sources[verts[i.get("source")[1:]]]
            counts = [int(x) for x in t.find(f"{NS}vcount").text.split()]
            idx = [int(x) for x in t.find(f"{NS}p").text.split()]
            at = 0
            for c in counts:
                ring = []
                for q in range(c):
                    n = idx[at + q * stride + off]
                    ring.append((pos[3 * n], pos[3 * n + 1], pos[3 * n + 2]))
                at += c * stride
                tris.append(ring)
        for t in mesh.findall(f"{NS}polygons"):
            inputs = t.findall(f"{NS}input")
            stride = max(int(i.get("offset")) for i in inputs) + 1
            for i in inputs:
                if i.get("semantic") == "VERTEX":
                    off = int(i.get("offset"))
                    pos = sources[verts[i.get("source")[1:]]]
            def ring_of(text):
                idx = [int(x) for x in text.split()]
                return [(pos[3 * idx[k]], pos[3 * idx[k] + 1], pos[3 * idx[k] + 2])
                        for k in range(off, len(idx), stride)]
            for ph in t.findall(f"{NS}ph"):
                outer = ring_of(ph.find(f"{NS}p").text)
                holes = [ring_of(h.text) for h in ph.findall(f"{NS}h")]
                tris.append(HoledRing(outer, holes))
        # Lines are wires, not surfaces: only their vertices count.
        pts = []
        for l in mesh.findall(f"{NS}lines"):
            inputs = l.findall(f"{NS}input")
            stride = max(int(i.get("offset")) for i in inputs) + 1
            for i in inputs:
                if i.get("semantic") == "VERTEX":
                    off = int(i.get("offset"))
                    src = sources[verts[i.get("source")[1:]]]
                    ptxt = l.find(f"{NS}p")
                    if ptxt is not None and ptxt.text:
                        idx = [int(x) for x in ptxt.text.split()]
                        for k in range(off, len(idx), stride):
                            n = idx[k]
                            pts.append((src[3 * n], src[3 * n + 1], src[3 * n + 2]))
        geoms[g.get("id")] = (tris, pts)
    lib = {n.get("id"): n for n in root.iter(f"{NS}node") if n.get("id")}
    tris, wires = [], []
    identity = [[float(i == j) for j in range(4)] for i in range(4)]

    def walk(node, m, depth=0):
        if depth > 40:
            return
        for x in node.findall(f"{NS}matrix"):
            v = [float(t) for t in x.text.split()]
            m = mat_mul(m, [v[0:4], v[4:8], v[8:12], v[12:16]])
        for g in node.findall(f"{NS}instance_geometry"):
            t, p = geoms.get(g.get("url")[1:], ([], []))
            for tri in t:
                ring = [apply(m, q) for q in tri]
                if isinstance(tri, HoledRing):
                    ring = HoledRing(ring, [[apply(m, q) for q in h] for h in tri.holes])
                tris.append(ring)
            for q in p:
                wires.append(apply(m, q))
        for i in node.findall(f"{NS}instance_node"):
            target = lib.get(i.get("url")[1:])
            if target is not None:
                walk(target, m, depth + 1)
        for c in node.findall(f"{NS}node"):
            walk(c, m, depth + 1)

    scene = root.find(f"{NS}scene/{NS}instance_visual_scene")
    vs = next(v for v in root.iter(f"{NS}visual_scene") if v.get("id") == scene.get("url")[1:])
    for n in vs.findall(f"{NS}node"):
        walk(n, identity)
    def scaled(t):
        ring = [tuple(c * scale for c in p) for p in t]
        if isinstance(t, HoledRing):
            return HoledRing(ring, [[tuple(c * scale for c in p) for p in h] for h in t.holes])
        return ring
    tris = [scaled(t) for t in tris]
    wires = [tuple(c * scale for c in p) for p in wires]
    return tris, wires


def obj_triangles(path):
    v, tris = [], []
    with open(path) as f:
        for line in f:
            if line.startswith("v "):
                x, y, z = (float(c) for c in line.split()[1:4])
                v.append((x, -z, y))  # the exporter writes Y up
            elif line.startswith("f "):
                ids = [int(t.split("/")[0]) for t in line.split()[1:]]
                ids = [i - 1 if i > 0 else len(v) + i for i in ids]
                for k in range(1, len(ids) - 1):
                    tris.append([v[ids[0]], v[ids[k]], v[ids[k + 1]]])
    return tris


def measure(tris):
    if not tris:
        return dict(n=0, area=0.0, bounds=None, verts=set())
    # Each side of a face is exported separately: count the surface once.
    seen = {}
    for t in tris:
        seen.setdefault((frozenset(tuple(round(c, 2) for c in p) for p in t), len(getattr(t, "holes", []))), t)
    tris = list(seen.values())
    pts = [p for t in tris for p in t]
    lo = [min(p[i] for p in pts) for i in range(3)]
    hi = [max(p[i] for p in pts) for i in range(3)]
    pts += [p for t in tris for h in getattr(t, "holes", []) for p in h]
    verts = {tuple(round(c, 2) for c in p) for p in pts}
    return dict(n=len(tris), area=sum(area(t) - sum(area(h) for h in getattr(t, "holes", [])) for t in tris), bounds=lo + hi, verts=verts)


def compare(ref, got):
    r = dict(tris=(ref["n"], got["n"]), area=(round(ref["area"], 3), round(got["area"], 3)))
    r["area_rel"] = abs(ref["area"] - got["area"]) / max(ref["area"], 1e-9)
    if ref["bounds"] and got["bounds"]:
        r["bounds_err"] = max(abs(a - b) for a, b in zip(ref["bounds"], got["bounds"]))
    else:
        r["bounds_err"] = None if ref["bounds"] == got["bounds"] else math.inf
    # Vertices of one set within 0.02 mm of the other; rounding buckets differ.
    def near(a, b):
        grid = set(b)
        miss = 0
        for p in a:
            if p in grid:
                continue
            if not any((round(p[0] + dx, 2), round(p[1] + dy, 2), round(p[2] + dz, 2)) in grid
                       for dx in STEPS for dy in STEPS for dz in STEPS):
                miss += 1
        return miss
    r["ref_missing"] = near(ref["verts"], got["verts"])
    r["got_extra"] = near(got["verts"], ref["verts"])
    return r



def dae_pairs(path):
    root = ET.parse(path).getroot()
    scale = float(root.find(f"{NS}asset/{NS}unit").get("meter")) * 1000
    geoms = {}
    for g in root.iter(f"{NS}geometry"):
        mesh = g.find(f"{NS}mesh")
        if mesh is None:
            continue
        src = {}
        for s in mesh.findall(f"{NS}source"):
            a = s.find(f"{NS}float_array")
            if a is not None and a.text:
                src[s.get("id")] = [float(x) for x in a.text.split()]
        verts = {}
        for v in mesh.findall(f"{NS}vertices"):
            for i in v.findall(f"{NS}input"):
                if i.get("semantic") == "POSITION":
                    verts[v.get("id")] = i.get("source")[1:]
        out = []
        for tag in ("polylist", "triangles", "polygons"):
            for t in mesh.findall(f"{NS}{tag}"):
                ins = {i.get("semantic"): i for i in t.findall(f"{NS}input")}
                if "TEXCOORD" not in ins:
                    continue
                stride = max(int(i.get("offset")) for i in ins.values()) + 1
                vo, to = int(ins["VERTEX"].get("offset")), int(ins["TEXCOORD"].get("offset"))
                pos = src[verts[ins["VERTEX"].get("source")[1:]]]
                uv = src[ins["TEXCOORD"].get("source")[1:]]
                texts = [p.text for p in t.iter(f"{NS}p")] + [h.text for h in t.iter(f"{NS}h")]
                for text in texts:
                    idx = [int(x) for x in text.split()]
                    for k in range(0, len(idx), stride):
                        v, c = idx[k + vo], idx[k + to]
                        out.append(((pos[3 * v], pos[3 * v + 1], pos[3 * v + 2]), (uv[2 * c], uv[2 * c + 1])))
        geoms[g.get("id")] = out
    lib = {n.get("id"): n for n in root.iter(f"{NS}node") if n.get("id")}
    pairs = []
    identity = [[float(i == j) for j in range(4)] for i in range(4)]

    def walk(node, m, depth=0):
        if depth > 40:
            return
        for x in node.findall(f"{NS}matrix"):
            v = [float(t) for t in x.text.split()]
            m = mat_mul(m, [v[0:4], v[4:8], v[8:12], v[12:16]])
        for g in node.findall(f"{NS}instance_geometry"):
            for p, uv in geoms.get(g.get("url")[1:], []):
                w = tuple((m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]) * scale for r in range(3))
                pairs.append((w, uv))
        for i in node.findall(f"{NS}instance_node"):
            t = lib.get(i.get("url")[1:])
            if t is not None:
                walk(t, m, depth + 1)
        for c in node.findall(f"{NS}node"):
            walk(c, m, depth + 1)

    url = root.find(f"{NS}scene/{NS}instance_visual_scene").get("url")[1:]
    vs = next(v for v in root.iter(f"{NS}visual_scene") if v.get("id") == url)
    for n in vs.findall(f"{NS}node"):
        walk(n, identity)
    return pairs


def cross(a, b):
    return (a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0])


def dot(a, b):
    return sum(x * y for x, y in zip(a, b))


def frame(normal, d):
    n = normal
    a = [abs(x) for x in n]
    axis = (1, 0, 0) if a[0] <= a[1] and a[0] <= a[2] else (0, 1, 0) if a[1] <= a[2] else (0, 0, 1)
    k = dot(axis, n)
    u = tuple(axis[i] - n[i] * k for i in range(3))
    l = math.sqrt(dot(u, u))
    u = tuple(x / l for x in u)
    return tuple(-d * x for x in n), u, cross(n, u)


def compose(a, b):
    # 12 numbers: columns x, y, z, translation.
    col = lambda t, i: t[3 * i:3 * i + 3]
    def vec(t, v):
        return tuple(t[r] * v[0] + t[3 + r] * v[1] + t[6 + r] * v[2] for r in range(3))
    out = []
    for i in range(3):
        out += vec(a, col(b, i))
    tr = vec(a, col(b, 3))
    out += [tr[r] + a[9 + r] for r in range(3)]
    return out


def place(t, p):
    return tuple(t[r] * p[0] + t[3 + r] * p[1] + t[6 + r] * p[2] + t[9 + r] for r in range(3))


def key(ref):
    return (ref["idx"], ref["version"])


def native_pairs(doc):
    tiles = {}
    for ref, m in doc["materials"]["items"]:
        if m.get("texture"):
            tiles[key(ref)] = m["texture"]["size_mm"]
    defs = {key(r): e for r, e in doc["definitions"]}
    pairs = []

    def visit(ent, t, depth=0):
        mesh = ent["mesh"]
        verts = mesh["vertices"]
        for f in mesh["faces"]:
            sides = f["sides"]
            origin, u, v = frame(f["plane"]["normal"], f["plane"]["d"])
            for mat, uvm in ((sides.get("material_front"), sides.get("uv_front")), (sides.get("material_back"), sides.get("uv_back"))):
                if not mat or key(mat) not in tiles or not uvm:
                    continue
                tile = tiles[key(mat)]
                for ring in f["loops"]:
                    for e in ring:
                        rev = e >= 2147483648
                        edge = mesh["edges"][e - 2147483648 if rev else e]
                        i = edge["b"] if rev else edge["a"]
                        p = verts[i]
                        r = tuple(p[k] - origin[k] for k in range(3))
                        x, y = dot(r, u), dot(r, v)
                        if "Projective" in uvm:
                            m = uvm["Projective"]
                            h = [m[c] * x + m[3 + c] * y + m[6 + c] for c in range(3)]
                            q = (h[0] / h[2] / tile[0], h[1] / h[2] / tile[1])
                        elif "Affine" in uvm:
                            continue
                        else:
                            continue
                        pairs.append((place(t, p), q))
        if depth > 40:
            return
        for kind in ("groups", "instances"):
            for _, g in ent.get(kind, []):
                visit(defs[key(g["definition"])]["entities"], compose(t, g["transform"]), depth + 1)

    visit(doc["model"], [1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0])
    return pairs


def uv_contradictions(ref, got):
    """Reference corners where the document places other coordinates."""
    index = {}
    for p, uv in got:
        index.setdefault(tuple(round(c) for c in p), []).append(uv)
    wrong = 0
    for p, uv in ref:
        near = [q for dx in (-1, 0, 1) for dy in (-1, 0, 1) for dz in (-1, 0, 1)
                for q in index.get((round(p[0]) + dx, round(p[1]) + dy, round(p[2]) + dz), ())]
        same = any(min((q[0] - uv[0]) % 1, (uv[0] - q[0]) % 1) < 0.004
                   and min((q[1] - uv[1]) % 1, (uv[1] - q[1]) % 1) < 0.004 for q in near)
        wrong += bool(near) and not same
    return wrong


STEPS = [round(k * 0.01, 2) for k in range(-3, 4)]


def run(*args):
    try:
        p = subprocess.run([CLI, *args], capture_output=True, text=True, env=ENV, timeout=280)
    except subprocess.TimeoutExpired:
        return -999, {}
    try:
        return p.returncode, json.loads(p.stdout)
    except ValueError:
        return p.returncode, {}


def version(path):
    with open(path, "rb") as f:
        found = HEADER.search(f.read(4096))
    return int(found.group(1).replace(b"\x00", b"").split(b".")[0]) if found else 0


def summary(info):
    out = info.get("output", {})
    counts = out.get("counts", {})
    model, inner = counts.get("model", {}), counts.get("definitions_total", {})
    totals = {k: model.get(k, 0) + inner.get(k, 0) for k in COUNTS[:7]}
    totals.update({k: counts.get(k) for k in COUNTS[7:]})
    return totals


def message(doc):
    error = doc.get("error")
    return str(error.get("message") if isinstance(error, dict) else error)[:200]


def canonical(path, d):
    out = os.path.join(d, "canonical.json")
    run("export", "-f", path, out, "--format", "json", "--canonical")
    with open(out, encoding="utf-8") as f:
        return json.load(f)


def names(doc):
    return {
        "material": [m["name"] for _, m in doc["materials"]["items"] if m["is_default_for"] is None],
        "tag": [t["name"] for ref, t in doc["tags"]["tags"] if ref != doc["tags"]["untagged"]],
        "scene": [s["name"] for _, s in doc["scenes"]["items"]],
        "component": [e["name"] for _, e in doc["definitions"] if not e["is_internal_group"]],
    }


def edits(native, base, d):
    """A batch editing `native`, the counts it leads to and its renames."""
    lines = [f"line --points {FAR[0]} --points {FAR[1]}"]
    want = dict(base, edges=base["edges"] + 1)
    code, found = run("query", "face", "--fields", "pid", "-f", native, "--no-save", "--json")
    faces = found.get("output", [])
    if code == 0 and faces:
        lines.append(f"delete --targets '@{faces[0]['pid']}'")
        want["faces"] -= 1
    commands = {
        "material": "material-edit --material '{}' --name '{}'",
        "tag": "tag-rename --tag '{}' --new-name '{}'",
        "scene": "scene-rename --scene '{}' --new-name '{}'",
        "component": "component-rename --definition '{}' --new-name '{}'",
    }
    renamed = {}
    for kind, present in names(canonical(native, d)).items():
        usable = [n for n in present if "'" not in n and n.strip() == n]
        if usable:
            renamed[kind] = (usable[0], f"Renamed {kind}")
            lines.append(commands[kind].format(*renamed[kind]))
    script = os.path.join(d, "edits.plyrb")
    with open(script, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    return script, want, renamed


def chain(path):
    """Problems of one model in the acceptance chain."""
    with tempfile.TemporaryDirectory() as d:
        src, native, restored, edited = (os.path.join(d, n) for n in ("m.skp", "w.plyr", "r.skp", "e.plyr"))
        shutil.copy(path, src)
        code, doc = run("info", "-f", src, "--json", "--no-save")
        if code:
            return [f"open: {message(doc)}"]
        base = summary(doc)
        code, doc = run("repair", "-f", src, "-o", native, "--json")
        if code or not os.path.exists(native):
            return [f"native save: {message(doc)}"]
        problems = []
        code, doc = run("info", "-f", native, "--json", "--no-save")
        if code or summary(doc) != base:
            problems.append("native reopen differs")
        code, doc = run("repair", "-f", native, "-o", restored, "--json")
        if code or not filecmp.cmp(src, restored, shallow=False):
            problems.append("unchanged restoration differs")
        script, want, renamed = edits(native, base, d)
        code, doc = run("batch", script, "-f", native, "-o", edited, "--json")
        if code:
            return problems + [f"edit: {message(doc)}"]
        for writer, extra in (("2017", []), ("2026", ["--skp-version", "2026"])):
            out = os.path.join(d, writer + ".skp")
            code, doc = run("export", "-f", edited, out, *extra, "--no-save", "--json")
            if code:
                problems.append(f"write {writer}: {message(doc)}")
                continue
            code, doc = run("info", "-f", out, "--json", "--no-save")
            if code:
                problems.append(f"reopen {writer}: {message(doc)}")
                continue
            got = summary(doc)
            problems += [f"{writer} {k}: {want[k]} -> {got[k]}" for k in COUNTS if got[k] != want[k]]
            result = names(canonical(out, d))
            for kind, (old, new) in renamed.items():
                if new not in result[kind] or old in result[kind]:
                    problems.append(f"{writer} {kind}: rename of {old!r} lost")
    return problems


def surfaces(src, d, name):
    out = os.path.join(d, name + ".obj")
    code, _ = run("export", "-f", src, out, "--include-hidden", "--no-save", "--json")
    return measure(obj_triangles(out)) if code == 0 and os.path.exists(out) else None


def oracle(job):
    """Mismatches of one model against its original COLLADA export."""
    root, year, name = job
    reference = measure(dae_triangles(os.path.join(root, "2017", name + ".dae"))[0])
    problems = []
    with tempfile.TemporaryDirectory() as d:
        src, native, edited = (os.path.join(d, n) for n in ("m.skp", "w.plyr", "e.plyr"))
        shutil.copy(os.path.join(root, year, name + ".skp"), src)
        run("repair", "-f", src, "-o", native)
        run("tool", "line", "--points", FAR[0], "--points", FAR[1], "-f", native, "-o", edited)
        stages = [("import", src)]
        for writer, extra in (("2017", []), ("2026", ["--skp-version", "2026"])):
            out = os.path.join(d, writer + ".skp")
            run("export", "-f", edited, out, *extra, "--no-save")
            stages.append((writer, out))
        for stage, path in stages:
            got = surfaces(path, d, stage) if os.path.exists(path) else None
            if got is None:
                problems.append(f"{stage}: not readable")
                continue
            c = compare(reference, got)
            if not (c["area_rel"] < 1e-4 and (c["bounds_err"] is None or c["bounds_err"] < 0.05) and c["ref_missing"] == 0):
                problems.append(f"{stage}: area {c['area']} bounds {c['bounds_err']} missing {c['ref_missing']}")
        pairs = dae_pairs(os.path.join(root, "2017", name + ".dae"))
        for stage, path in stages if pairs else ():
            wrong = uv_contradictions(pairs, native_pairs(canonical(path, d)))
            if wrong:
                problems.append(f"{stage}: {wrong} of {len(pairs)} texture corners differ")
    return problems


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("corpus")
    parser.add_argument("--oracle")
    parser.add_argument("--jobs", type=int, default=8)
    args = parser.parse_args()
    models = sorted(
        os.path.join(d, n)
        for d, _, files in os.walk(args.corpus)
        for n in files
        if n.lower().endswith((".skp", ".skb"))
    )
    models = [m for m in models if version(m) >= 13]
    failed = 0
    with cf.ThreadPoolExecutor(args.jobs) as pool:
        for model, problems in zip(models, pool.map(chain, models)):
            failed += bool(problems)
            for problem in problems:
                print(f"{os.path.relpath(model, args.corpus)}: {problem}")
        print(f"chain: {len(models) - failed} of {len(models)} models pass")
        if args.oracle:
            names = sorted(f[:-4] for f in os.listdir(os.path.join(args.oracle, "2017")) if f.endswith(".dae"))
            jobs = [(args.oracle, y, n) for y in ("2017", "2026") for n in names
                    if os.path.exists(os.path.join(args.oracle, y, n + ".skp"))]
            exact = 0
            for job, problems in zip(jobs, pool.map(oracle, jobs)):
                exact += not problems
                for problem in problems:
                    print(f"oracle {job[1]}/{job[2]}: {problem}")
            print(f"oracle: {exact} of {len(jobs)} models match")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
