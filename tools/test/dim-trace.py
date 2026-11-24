#!/usr/bin/env python3
"""Flicker-free dimming: what the backlight does during one brightness step.

Moves KDE's brightness from START to TARGET percent through org.kde.ScreenBrightness (what the
keys and the slider do) while sampling the panel's backlight about every millisecond, and prints
each change with its time. Below the floor the backlight must go to the floor and stay: a dip
below the floor that lasts more than a few milliseconds is the "pulse" (darker, then back).

    dim-trace.py START TARGET [RESTORE]      (percent; RESTORE is set afterwards, default START)
"""
import subprocess
import sys
import threading
import time

BL = "/sys/class/backlight/intel_backlight"
DISPLAY = "/org/kde/ScreenBrightness/display4"


def kde(pct):
    subprocess.run(
        ["busctl", "--user", "call", "org.kde.ScreenBrightness", DISPLAY,
         "org.kde.ScreenBrightness.Display", "SetBrightness", "iu", str(int(pct * 100)), "0"],
        check=True, capture_output=True)


def main():
    start, target = float(sys.argv[1]), float(sys.argv[2])
    restore = float(sys.argv[3]) if len(sys.argv) > 3 else start
    mx = int(open(f"{BL}/max_brightness").read())
    kde(start)
    time.sleep(1.5)
    events = []
    stop = threading.Event()

    def sample():
        f = open(f"{BL}/actual_brightness")
        last = None
        while not stop.is_set():
            f.seek(0)
            v = int(f.read())
            if v != last:
                events.append((time.time(), v * 100 / mx))
                last = v
            time.sleep(0.0005)

    th = threading.Thread(target=sample)
    th.start()
    time.sleep(0.1)
    mark = time.monotonic()
    kde(target)
    time.sleep(1.2)
    stop.set()
    th.join()
    print(f"backlight changes after the press at {target:g}% (ms, percent of max):")
    base = events[0][0] if events else 0
    for t, v in events:
        print(f"  {time.strftime('%H:%M:%S', time.localtime(t))}.{int(t % 1 * 1000):03d}  +{(t - base) * 1000:7.1f} ms  {v:6.2f}")
    floor_dip = [e for e in events if e[1] < 29.5]
    print(f"lowest backlight seen: {min(v for _, v in events):.2f} %")
    if floor_dip:
        span = floor_dip[-1][0] - floor_dip[0][0]
        print(f"time below the floor (first to last sample): {span:.0f} ms")
    kde(restore)


main()
