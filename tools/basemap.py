"""Rebuild data/basemap.json from OpenStreetMap: river Tormes and main roads, simplified.

    curl -A salbus-map-build --data-urlencode 'data=[out:json][timeout:60];(way["waterway"="river"](40.925,-5.745,40.995,-5.615);way["highway"~"^(motorway|trunk|primary|secondary|tertiary)$"](40.925,-5.745,40.995,-5.615););out geom;' https://overpass-api.de/api/interpreter -o osm.json
    python3 tools/basemap.py osm.json data/basemap.json

Data © OpenStreetMap contributors, ODbL.
"""
import json
import sys

src, dst = sys.argv[1], sys.argv[2]
ways = json.load(open(src))["elements"]


def simplify(pts, eps):
    """Douglas-Peucker."""
    if len(pts) < 3:
        return pts
    (x1, y1), (x2, y2) = pts[0], pts[-1]
    dx, dy = x2 - x1, y2 - y1
    norm = (dx * dx + dy * dy) ** 0.5 or 1e-12
    dist = [abs(dy * (x - x1) - dx * (y - y1)) / norm for x, y in pts[1:-1]]
    i = max(range(len(dist)), key=dist.__getitem__)
    if dist[i] <= eps:
        return [pts[0], pts[-1]]
    return simplify(pts[: i + 2], eps)[:-1] + simplify(pts[i + 1 :], eps)


def lines(wanted, eps=0.00012):  # about 12 m
    out = []
    for way in ways:
        if not wanted(way["tags"]):
            continue
        pts = simplify([(g["lon"], g["lat"]) for g in way["geometry"]], eps)
        if len(pts) >= 2:
            out.append([(round(x, 5), round(y, 5)) for x, y in pts])
    return out


base = {
    "river": lines(lambda t: t.get("waterway") == "river" and "Tormes" in t.get("name", "")),
    "main": lines(lambda t: t.get("highway") in ("motorway", "trunk", "primary", "secondary")),
    "minor": lines(lambda t: t.get("highway") == "tertiary"),
}
for name, found in base.items():
    print(name, len(found), "lines", sum(map(len, found)), "points")
json.dump(base, open(dst, "w"), separators=(",", ":"))
