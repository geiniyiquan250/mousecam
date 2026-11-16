"""Generate human-ish pointer tracks for judging camera smoothing.

A perfect circle flatters every filter: it has no tremor, no speed variation and
no aiming corrections. Real hands have all three, so the test tracks do too.
"""
import math, random, json, os

random.seed(20261104)

def tremor(t, amp=1.0):
    # physiological tremor: 8-12 Hz, sub-pixel to a couple of pixels
    return (amp * 0.8 * math.sin(2 * math.pi * 8.3 * t + 1.7)
            + amp * 0.5 * math.sin(2 * math.pi * 11.9 * t + 4.1)
            + random.gauss(0, 0.35))

def ease(t):
    t = max(0.0, min(1.0, t))
    return t * t * (3 - 2 * t)

def circle_track(seconds=12.0, cx=1280.0, cy=720.0, r=70.0, hz=1.6):
    """Loitering: the pointer circles in a small area, radius and rate drifting."""
    out = []
    n = int(seconds * 250)
    for i in range(n):
        t = i / 250.0
        if t < 1.0 or t >= 9.0:
            base = (cx, cy)
        else:
            rr = r * (1.0 + 0.18 * math.sin(2 * math.pi * 0.23 * t))
            ff = hz * (1.0 + 0.12 * math.sin(2 * math.pi * 0.17 * t + 0.9))
            # integrate the drift so the angle stays continuous
            a = 2 * math.pi * ff * (t - 1.0)
            base = (cx + rr * math.cos(a), cy + rr * math.sin(a))
        out.append((t, base[0] + tremor(t * 1.0, 1.4), base[1] + tremor(t * 1.0 + 0.37, 1.4)))
    return out

def wander_track(seconds=12.0, w=2560, h=1440):
    """Ordinary use: move, stop, aim, overshoot, correct."""
    out = []
    n = int(seconds * 250)
    pos = [w * 0.5, h * 0.5]
    start = list(pos)
    target = list(pos)
    seg_start = 0.0
    seg_len = 0.9
    phase = 0.0
    for i in range(n):
        t = i / 250.0
        el = t - seg_start
        if el >= seg_len:
            # pick the next target, with a short pause between about a third of them
            start = list(pos)
            target = [random.uniform(w * 0.12, w * 0.88), random.uniform(h * 0.12, h * 0.88)]
            seg_start = t
            seg_len = random.uniform(0.45, 1.3) + (0.35 if random.random() < 0.33 else 0.0)
            phase = ease(el / max(seg_len, 1e-3))
        p = ease(el / max(seg_len, 1e-3))
        # humans overshoot a little and then come back
        over = 0.04 * math.sin(math.pi * min(1.0, p)) if p < 1.0 else 0.0
        x = start[0] + (target[0] - start[0]) * (p + over)
        y = start[1] + (target[1] - start[1]) * (p + over)
        out.append((t, x + tremor(t, 1.0), y + tremor(t + 0.51, 1.0)))
    return out

def write(clip_dir, points):
    lines = ["# MouseCam track v1 fps=30"]
    for t, x, y in points:
        lines.append("%.5f,%d,%d,0" % (t, int(round(x)), int(round(y))))
    open(os.path.join(clip_dir, "track.csv"), "w", encoding="utf-8").write("\n".join(lines) + "\n")
    meta = json.load(open(os.path.join(clip_dir, "meta.json"), encoding="utf-8"))
    meta["duration"] = points[-1][0]
    meta["frames"] = int(points[-1][0] * meta["fps"])
    json.dump(meta, open(os.path.join(clip_dir, "meta.json"), "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    return len(points)

root = r"E:\RAYS\Desktop\workspace\mousecam\rust\mc-engine\target"
for name, gen in (("orbit", circle_track), ("wander", wander_track)):
    d = os.path.join(root, "track-" + name)
    print(name, write(d, gen()), "samples")
