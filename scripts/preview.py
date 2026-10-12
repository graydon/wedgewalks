#!/usr/bin/env python3
"""Quick PNG preview of a city's output: graph (car-free green, road grey),
rings, arc cut points and stops. Usage: scripts/preview.py out/<city> [ring#]"""
import json, math, sys
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.collections import LineCollection

d = sys.argv[1]
want = int(sys.argv[2]) if len(sys.argv) > 2 else 1
rings = json.load(open(f"{d}/rings.geojson"))["features"]
arcs = json.load(open(f"{d}/arcs.geojson"))["features"]
graph = json.load(open(f"{d}/graph.geojson"))["features"]
center = rings[0]["geometry"]["coordinates"]
k = math.cos(math.radians(center[1]))
xy = lambda c: ((c[0] - center[0]) * k * 111.32, (c[1] - center[1]) * 111.32)

fig, ax = plt.subplots(figsize=(12, 12))
for kind, color, lw in (("road", "#cccccc", 0.3), ("car_free", "#7fbf7f", 0.5)):
    segs = [[xy(c) for c in f["geometry"]["coordinates"]] for f in graph if f["properties"]["kind"] == kind]
    ax.add_collection(LineCollection(segs, colors=color, linewidths=lw))
colors = ["#d62728", "#1f77b4", "#ff7f0e", "#9467bd", "#8c564b", "#e377c2"]
for f in arcs:
    p = f["properties"]
    if f["geometry"]["type"] == "LineString" and p["ring"] == want:
        pts = [xy(c) for c in f["geometry"]["coordinates"]]
        ax.plot(*zip(*pts), color=colors[(p["arc"] - 1) % len(colors)], lw=3)
        ax.plot(*pts[0], "ko", ms=6)
        ax.annotate(str(p["arc"]), pts[len(pts) // 2], fontsize=14, weight="bold")
    elif f["geometry"]["type"] == "Point" and p["ring"] == want:
        ax.plot(*xy(f["geometry"]["coordinates"]), "bs", ms=5)
ax.plot(0, 0, "k*", ms=15)
r = [f for f in rings if f["properties"].get("rank") == want][0]["properties"]
ax.set_title(f"{d}: ring {want}, {r['length_km']} km, road {r['road_fraction']*100:.0f}%, ok={r['ok']} {r['problems']}")
ax.set_aspect("equal"); ax.set_xlim(-16, 16); ax.set_ylim(-16, 16)
out = f"{d}/ring{want}.png"
fig.savefig(out, dpi=80, bbox_inches="tight")
print(out)
