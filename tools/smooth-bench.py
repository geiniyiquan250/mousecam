import json, subprocess, math, os, sys
ROOT = r"E:\RAYS\Desktop\workspace\mousecam\rust\mc-engine\target"

def bake(clip, edit):
    out = os.path.join(ROOT, "_b.json")
    subprocess.run(["node", os.path.join(ROOT, "_b.mjs"), clip, out, edit], check=True)
    return json.load(open(out, encoding="utf-8"))

def track(path):
    pts = []
    for line in open(path, encoding="utf-8"):
        if line.startswith("#"): continue
        f = line.strip().split(",")
        if len(f) >= 3: pts.append((float(f[0]), int(f[1]), int(f[2])))
    return pts

def metrics(path, tr):
    n = len(path)
    xs = [p[0] for p in path]; ys = [p[1] for p in path]
    # wobble: deviation of the camera from its own 0.5 s moving average
    win = 15
    wob = []
    for i in range(win, n - win):
        ax = sum(xs[i-win:i+win+1]) / (2*win+1)
        ay = sum(ys[i-win:i+win+1]) / (2*win+1)
        wob.append(math.hypot(xs[i]-ax, ys[i]-ay))
    wob.sort()
    p95 = wob[int(len(wob)*0.95)] if wob else 0.0
    rms = math.sqrt(sum(w*w for w in wob)/len(wob)) if wob else 0.0
    lag = 0.0
    for i in range(2, n):
        t = i / 30.0
        c = min(tr, key=lambda q: abs(q[0]-t))
        lag += math.hypot(xs[i]-c[1], ys[i]-c[2])
    travel = sum(math.hypot(xs[i]-xs[i-1], ys[i]-ys[i-1]) for i in range(1, n))
    return travel, lag/(n-2), rms, p95

base = "zoom 1.0\nexpand 1.2\nthreshold 3400\naspect 0\nwidth 1080\nsmooth 66\nresp 35\nfollow 1.0\nlook 60\n"
configs = [("旧行为 (dz 0)", base + "dz 0\n"),
           ("新默认 (dz 7)", base + "dz 7\n")]
for label, edit in configs:
    print("== %s ==" % label)
    for name in ("orbit", "wander"):
        clip = os.path.join(ROOT, "track-" + name)
        path = bake(clip, edit)
        tr = track(os.path.join(clip, "track.csv"))
        travel, lag, rms, p95 = metrics(path, tr)
        print("  %-8s 波动 rms %5.1f px  p95 %5.1f px   行程 %6.0f   离光标 %5.1f"
              % ("画圈" if name == "orbit" else "普通移动", rms, p95, travel, lag))
