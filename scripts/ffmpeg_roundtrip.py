#!/usr/bin/env python3
"""Stress-test frame encodings against lossy video compression.

For each configuration: encode N random-payload frames to PNG, turn them into a video with
ffmpeg (lossy), pull the frames back out, decode, and compare with the original payloads.

Usage: scripts/ffmpeg_roundtrip.py [--quick] [--modes gray,rgb] [--levels ..] [--scales ..] [--codecs ..] [--rates ..] [--ecc ..] [--frames N] [--hold N] [--workdir DIR]
Requires: ffmpeg (with libx264/libx265) and a release build of this project.
"""
import argparse, itertools, os, shutil, subprocess, sys, tempfile
from concurrent.futures import ThreadPoolExecutor

BIN = os.path.join(os.path.dirname(__file__), "..", "target", "release", "youtube_video_storage")
W, H = 1920, 1080
FRAMES, HOLD = 3, 15   # each frame is held HOLD video frames (set by --frames/--hold)


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def codec_args(codec, rate):
    if codec == "x264":
        return ["-c:v", "libx264", "-preset", "medium", "-b:v", rate]
    return ["-c:v", "libx265", "-preset", "fast", "-b:v", rate, "-x265-params", "log-level=error"]


def trial(cfg, root):
    mode, levels, scale, codec, rate, ecc = cfg
    name = f"{mode}_{levels}_s{scale}_{codec}_{rate}_e{ecc}"
    d = os.path.join(root, name)
    os.makedirs(d)
    flags = ["--size", f"{W // scale}x{H // scale}", "--levels", str(levels), "--ecc", str(ecc)]
    if mode == "rgb":
        flags.append("--rgb")
    cap = int(run([BIN, "capacity", *flags]).stdout)
    payloads = []
    for i in range(FRAMES):
        data = os.urandom(cap)
        payloads.append(data)
        with open(f"{d}/in{i}.bin", "wb") as f:
            f.write(data)
        r = run([BIN, "encode", f"{d}/in{i}.bin", f"{d}/f_{i + 1}.png", *flags, "--scale", str(scale)])
        if r.returncode:
            return name, cap, "encode failed: " + r.stderr.strip(), 0
    vid = f"{d}/v.mp4"
    r = run(["ffmpeg", "-y", "-loglevel", "error", "-framerate", str(30 // HOLD), "-i", f"{d}/f_%d.png", "-r", "30",
             *codec_args(codec, rate), "-pix_fmt", "yuv420p", vid])
    if r.returncode:
        return name, cap, "ffmpeg failed: " + r.stderr.strip()[-200:], 0
    sel = "+".join(f"eq(n\\,{i * HOLD + HOLD // 2})" for i in range(FRAMES))
    run(["ffmpeg", "-y", "-loglevel", "error", "-i", vid, "-vf", f"select='{sel}'", "-fps_mode", "passthrough",
         f"{d}/o_%d.png"])
    ok, repaired, notes = 0, 0, []
    for i in range(FRAMES):
        r = run([BIN, "decode", f"{d}/o_{i + 1}.png", f"{d}/out{i}.bin", "--scale", str(scale)])
        if r.returncode == 0 and open(f"{d}/out{i}.bin", "rb").read() == payloads[i]:
            ok += 1
            repaired += int(r.stdout.split("(")[1].split()[0])
        else:
            notes.append(r.stderr.strip().replace("error: ", "")[:60] or "payload differs")
    size = os.path.getsize(vid)
    shutil.rmtree(d)
    return name, cap, f"{ok}/{FRAMES} ok, repaired~{repaired // max(ok, 1)}B/frame, video {size // 1024}KiB" + (
        f"  [{notes[0]}]" if notes else ""), ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true")
    ap.add_argument("--workdir")
    ap.add_argument("--frames", type=int, default=3, help="distinct frames per trial")
    ap.add_argument("--hold", type=int, default=15,
                    help="video frames each image is shown for (1 = every video frame is new data)")
    ap.add_argument("--modes", default="gray,rgb")
    ap.add_argument("--levels", default="256,16,4,2")
    ap.add_argument("--scales", default="8,4")
    ap.add_argument("--codecs", default="x264,x265")
    ap.add_argument("--rates", default="8M,3M")
    ap.add_argument("--ecc", default="32")
    a = ap.parse_args()
    global FRAMES, HOLD
    FRAMES, HOLD = a.frames, a.hold
    if not os.path.exists(BIN):
        sys.exit("build first: cargo build --release")
    ints = lambda t: [int(x) for x in t.split(",")]
    if a.quick:
        grid = itertools.product(["gray"], [256, 16, 4, 2], [8], ["x264"], ["5M"], [32])
    else:
        grid = itertools.product(a.modes.split(","), ints(a.levels), ints(a.scales), a.codecs.split(","),
                                 a.rates.split(","), ints(a.ecc))
    grid = list(grid)
    root = a.workdir or tempfile.mkdtemp(prefix="yvs_")
    os.makedirs(root, exist_ok=True)
    print(f"{len(grid)} configurations, workdir {root}\n{'config':44} {'cap B/frame':>11}  result")
    with ThreadPoolExecutor(os.cpu_count() or 2) as ex:
        for name, cap, res, _ in ex.map(lambda c: trial(c, root), grid):
            print(f"{name:44} {cap:>11}  {res}", flush=True)


if __name__ == "__main__":
    main()
