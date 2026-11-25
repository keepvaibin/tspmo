#!/usr/bin/env python3
"""Profile holds through the power-profiles API, on the real daemon (needs python-gobject).

A hold lasts as long as the connection that made it, so this keeps one open: a performance hold
wins over balanced, is listed in ActiveProfileHolds, and is dropped when released and when the
connection closes. A power-saver hold is Eco+. Prints `name PASS|FAIL`; exits 1 on any
FAIL. Leaves the mode as it found it.
"""
import subprocess
import sys
import time

from gi.repository import Gio, GLib

NAME = "org.freedesktop.UPower.PowerProfiles"
PATH = "/org/freedesktop/UPower/PowerProfiles"
fails = 0


def check(name, ok):
    global fails
    fails += 0 if ok else 1
    print(f"{name:<52} {'PASS' if ok else 'FAIL'}")


def proxy(bus):
    return Gio.DBusProxy.new_sync(bus, Gio.DBusProxyFlags.NONE, None, NAME, PATH, NAME, None)


def active(p):
    return p.get_cached_property("ActiveProfile").unpack()


def holds(bus):
    p = proxy(bus)
    return len(p.get_cached_property("ActiveProfileHolds").unpack())


def tspmo(*a):
    return subprocess.run(["tspmo", *a], capture_output=True, text=True).stdout.strip()


start = tspmo("profile")
try:
    tspmo("profile", "balanced")
    time.sleep(1.5)
    bus = Gio.bus_get_sync(Gio.BusType.SYSTEM, None)
    p = proxy(bus)
    cookie = p.call_sync("HoldProfile", GLib.Variant("(sss)", ("performance", "e2e", "tspmo-test")), 0, -1, None).unpack()[0]
    time.sleep(1.5)
    check("performance hold wins over balanced", tspmo("profile") == "performance")
    check("hold listed in ActiveProfileHolds", holds(bus) == 1)
    p.call_sync("ReleaseProfile", GLib.Variant("(u)", (cookie,)), 0, -1, None)
    time.sleep(1.5)
    check("release returns to balanced", tspmo("profile") == "balanced")

    # A hold dies with its connection.
    bus2 = Gio.DBusConnection.new_for_address_sync(
        Gio.dbus_address_get_for_bus_sync(Gio.BusType.SYSTEM, None),
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
        None, None)
    proxy(bus2).call_sync("HoldProfile", GLib.Variant("(sss)", ("performance", "e2e", "tspmo-test")), 0, -1, None)
    time.sleep(1.5)
    check("second client's hold applies", tspmo("profile") == "performance")
    bus2.close_sync(None)
    time.sleep(2)
    check("hold dropped when its client leaves", tspmo("profile") == "balanced")

    # A power-saver hold is Eco+; a performance hold wins; release returns to balanced.
    tspmo("profile", "balanced")
    time.sleep(2)
    c1 = p.call_sync("HoldProfile", GLib.Variant("(sss)", ("power-saver", "e2e", "tspmo-test")), 0, -1, None).unpack()[0]
    time.sleep(1.5)
    check("power-saver hold is eco-plus", tspmo("profile") == "eco-plus")
    c2 = p.call_sync("HoldProfile", GLib.Variant("(sss)", ("performance", "e2e", "tspmo-test")), 0, -1, None).unpack()[0]
    time.sleep(2)
    check("performance hold wins over eco-plus", tspmo("profile") == "performance")
    p.call_sync("ReleaseProfile", GLib.Variant("(u)", (c2,)), 0, -1, None)
    p.call_sync("ReleaseProfile", GLib.Variant("(u)", (c1,)), 0, -1, None)
    time.sleep(2)
    check("release returns to balanced", tspmo("profile") == "balanced")
finally:
    tspmo("profile", start)
sys.exit(1 if fails else 0)
