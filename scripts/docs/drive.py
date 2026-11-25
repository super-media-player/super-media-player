#!/usr/bin/env python3
"""Mouse and keyboard for the app on a virtual X display (capture.sh).

Actions, in order:
  focus            give the player's window the keyboard
  key:Ctrl+k       press a key (X names: Return, space, Right, bracketleft…)
  type:some text   type it
  move:x,y         put the mouse there      glide:x,y[,secs]  move it there
  click[:x,y]      click (after gliding)    dbl:x,y  rclick:x,y
  wheel:n          scroll n notches down (negative: up)
  drag:x,y,secs    hold the button and glide
  sleep:secs
"""
import ctypes, os, re, subprocess, sys, time

name = os.environ.get("DISPLAY", ":77")
x = ctypes.CDLL("libX11.so.6")
t = ctypes.CDLL("libXtst.so.6")
x.XOpenDisplay.restype = ctypes.c_void_p
d = ctypes.c_void_p(x.XOpenDisplay(name.encode()))
if not d:
    sys.exit(f"no display {name}")
x.XStringToKeysym.restype = ctypes.c_ulong
MODS = {"Ctrl": "Control_L", "Shift": "Shift_L", "Alt": "Alt_L"}
NAMED = {" ": "space", ".": "period", ",": "comma", "-": "minus", ":": "colon", "/": "slash", "'": "apostrophe"}
pos = [550, 340]


def code(n):
    return x.XKeysymToKeycode(d, ctypes.c_ulong(x.XStringToKeysym(n.encode())))


def key(k, mods=()):
    for m in mods:
        t.XTestFakeKeyEvent(d, code(m), 1, 0)
    t.XTestFakeKeyEvent(d, code(k), 1, 0)
    t.XTestFakeKeyEvent(d, code(k), 0, 0)
    for m in mods:
        t.XTestFakeKeyEvent(d, code(m), 0, 0)
    x.XFlush(d)
    time.sleep(0.05)


def focus():
    tree = subprocess.run(["xwininfo", "-root", "-tree", "-display", name], capture_output=True, text=True).stdout
    found = [
        (int(m[2]) * int(m[3]), int(m[1], 16))
        for m in re.finditer(r"(0x[0-9a-f]+) [^\n]*?  (\d+)x(\d+)\+", tree)
        if "has no name" not in m[0] and "xwininfo" not in m[0]
    ]
    if found:
        x.XSetInputFocus(d, ctypes.c_ulong(max(found)[1]), 2, 0)
        x.XFlush(d)
        time.sleep(0.2)


def move(a, b):
    pos[:] = [a, b]
    t.XTestFakeMotionEvent(d, -1, int(a), int(b), 0)
    x.XFlush(d)
    time.sleep(0.01)


def glide(a, b, secs=0.6):
    x0, y0 = pos
    n = max(2, int(secs * 50))
    for i in range(1, n + 1):
        u = i / n
        u = u * u * (3 - 2 * u)
        move(x0 + (a - x0) * u, y0 + (b - y0) * u)
        time.sleep(max(0, secs / n - 0.01))


def button(n, down):
    t.XTestFakeButtonEvent(d, n, down, 0)
    x.XFlush(d)
    time.sleep(0.05)


def click(n=1):
    button(n, 1)
    button(n, 0)


for a in sys.argv[1:]:
    c, _, v = a.partition(":")
    nums = [float(p) for p in v.split(",")] if c in ("move", "glide", "dbl", "rclick", "drag") or c == "click" and v else []
    if c == "key":
        p = v.split("+")
        key(p[-1], [MODS.get(m, m) for m in p[:-1]])
    elif c == "type":
        for ch in v:
            shifted = ch.isupper()
            key(NAMED.get(ch, ch), ["Shift_L"] if shifted or ch == ":" else [])
            time.sleep(0.07)
    elif c == "click":
        if nums:
            glide(*nums)
            time.sleep(0.15)
        click()
    elif c == "dbl":
        glide(*nums)
        click()
        click()
    elif c == "rclick":
        glide(*nums)
        click(3)
    elif c == "drag":
        button(1, 1)
        glide(*nums)
        button(1, 0)
    elif c == "focus":
        focus()
    elif c == "move":
        move(*nums)
    elif c == "glide":
        glide(*nums)
    elif c == "wheel":
        for _ in range(abs(int(v))):
            click(5 if int(v) > 0 else 4)
            time.sleep(0.12)
    elif c == "sleep":
        time.sleep(float(v))
    else:
        sys.exit(f"unknown action {a}")
x.XCloseDisplay(d)
