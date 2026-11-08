#!/usr/bin/env python3
"""Minimal org.freedesktop.login1 service for the recovery checks.

It serves the manager, user, and session objects that Kestrel probes, hands
out inhibitor descriptors, and emits PrepareForSleep when the control word
changes. It is not a logind implementation: only the calls these checks use
exist, and every access decision comes from the control file.

Environment:
  FAKE_LOGIND_BUS_ADDRESS  bus to serve on; defaults to the system bus
  FAKE_LOGIND_STATE_DIR    directory holding `control`, `inhibitors`, `ready`

Control words: allow, sleep, locked, deny. `sleep` sets PreparingForSleep and
emits PrepareForSleep(true); leaving it emits PrepareForSleep(false). `deny`
answers Inhibit with AccessDenied.
"""

import os
import socket
import sys

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib

MANAGER_PATH = "/org/freedesktop/login1"
SESSION_PATH = "/org/freedesktop/login1/session/_1"
USER_PATH = "/org/freedesktop/login1/user/_1000"
MANAGER_IFACE = "org.freedesktop.login1.Manager"
SESSION_IFACE = "org.freedesktop.login1.Session"
USER_IFACE = "org.freedesktop.login1.User"

NODE_XML = """<node>
  <interface name="org.freedesktop.login1.Manager">
    <method name="GetSession">
      <arg type="s" direction="in"/>
      <arg type="o" direction="out"/>
    </method>
    <method name="GetUser">
      <arg type="u" direction="in"/>
      <arg type="o" direction="out"/>
    </method>
    <method name="Inhibit">
      <arg type="s" direction="in"/>
      <arg type="s" direction="in"/>
      <arg type="s" direction="in"/>
      <arg type="s" direction="in"/>
      <arg type="h" direction="out"/>
    </method>
    <property name="NCurrentInhibitors" type="t" access="read"/>
    <property name="PreparingForSleep" type="b" access="read"/>
    <signal name="PrepareForSleep">
      <arg type="b"/>
    </signal>
  </interface>
  <interface name="org.freedesktop.login1.Session">
    <property name="Id" type="s" access="read"/>
    <property name="LockedHint" type="b" access="read"/>
  </interface>
  <interface name="org.freedesktop.login1.User">
    <property name="Display" type="o" access="read"/>
  </interface>
</node>"""

STATE_DIR = os.environ["FAKE_LOGIND_STATE_DIR"]
CONTROL = os.path.join(STATE_DIR, "control")
APPLIED = os.path.join(STATE_DIR, "applied")
COUNT = os.path.join(STATE_DIR, "inhibitors")
READY = os.path.join(STATE_DIR, "ready")

connection = None
node = Gio.DBusNodeInfo.new_for_xml(NODE_XML)
leases = []
preparing_for_sleep = False
locked = False
deny_inhibit = False
last_word = None


def read_word():
    try:
        with open(CONTROL, encoding="utf-8") as control:
            return control.read().strip() or "allow"
    except FileNotFoundError:
        return "allow"


def write_count():
    try:
        with open(COUNT, "w", encoding="utf-8") as count:
            count.write(f"{len(leases)}\n")
    except OSError:
        pass


def emit_property_changed(path, interface, name, value):
    connection.emit_signal(
        None,
        path,
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
        GLib.Variant("(sa{sv}as)", (interface, {name: value}, [])),
    )


def apply_word(word):
    global preparing_for_sleep, locked, deny_inhibit
    asleep = word == "sleep"
    changed = asleep != preparing_for_sleep
    locked_changed = (word == "locked") != locked
    preparing_for_sleep = asleep
    locked = word == "locked"
    deny_inhibit = word == "deny"
    if changed:
        connection.emit_signal(
            None,
            MANAGER_PATH,
            MANAGER_IFACE,
            "PrepareForSleep",
            GLib.Variant("(b)", (preparing_for_sleep,)),
        )
        emit_property_changed(
            MANAGER_PATH,
            MANAGER_IFACE,
            "PreparingForSleep",
            GLib.Variant("b", preparing_for_sleep),
        )
    if locked_changed:
        emit_property_changed(
            SESSION_PATH,
            SESSION_IFACE,
            "LockedHint",
            GLib.Variant("b", locked),
        )
    try:
        with open(APPLIED, "w", encoding="utf-8") as applied:
            applied.write(f"{word}\n")
    except OSError:
        pass


def watch_control():
    global last_word
    word = read_word()
    if word != last_word:
        last_word = word
        apply_word(word)
    return True


def release_lease(_channel, _condition, lease):
    if lease in leases:
        leases.remove(lease)
        write_count()
    try:
        lease.close()
    except OSError:
        pass
    return False


def on_method(_connection, _sender, path, interface, method, parameters, invocation):
    if interface != MANAGER_IFACE:
        invocation.return_dbus_error(
            "org.freedesktop.DBus.Error.UnknownInterface", interface
        )
        return
    if method == "GetSession":
        invocation.return_value(GLib.Variant("(o)", (SESSION_PATH,)))
    elif method == "GetUser":
        invocation.return_value(GLib.Variant("(o)", (USER_PATH,)))
    elif method == "Inhibit":
        acquire_inhibitor(invocation)
    else:
        invocation.return_dbus_error(
            "org.freedesktop.DBus.Error.UnknownMethod", f"{interface}.{method}"
        )


def acquire_inhibitor(invocation):
    if deny_inhibit:
        invocation.return_dbus_error(
            "org.freedesktop.DBus.Error.AccessDenied",
            "the fake logind denies Inhibit",
        )
        return
    kept, passed = socket.socketpair()
    kept.setblocking(False)
    fd_list = Gio.UnixFDList.new()
    index = fd_list.append(passed.fileno())
    passed.close()
    leases.append(kept)
    write_count()
    GLib.io_add_watch(
        kept, GLib.PRIORITY_DEFAULT, GLib.IO_HUP | GLib.IO_ERR, release_lease, kept
    )
    invocation.return_value_with_unix_fd_list(GLib.Variant("(h)", (index,)), fd_list)


def on_property(_connection, _sender, path, interface, name):
    if interface == MANAGER_IFACE:
        if name == "NCurrentInhibitors":
            return GLib.Variant("t", len(leases))
        if name == "PreparingForSleep":
            return GLib.Variant("b", preparing_for_sleep)
    elif interface == SESSION_IFACE:
        if name == "LockedHint":
            return GLib.Variant("b", locked)
        if name == "Id":
            return GLib.Variant("s", "_1")
    elif interface == USER_IFACE and name == "Display":
        return GLib.Variant("o", SESSION_PATH)
    return None


def main():
    global connection
    address = os.environ.get("FAKE_LOGIND_BUS_ADDRESS")
    if address:
        connection = Gio.DBusConnection.new_for_address_sync(
            address,
            Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
            | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
            None,
            None,
        )
    else:
        connection = Gio.bus_get_sync(Gio.BusType.SYSTEM, None)
    for path in (MANAGER_PATH, SESSION_PATH, USER_PATH):
        for interface in node.interfaces:
            connection.register_object(path, interface, on_method, on_property, None)
    Gio.bus_own_name_on_connection(
        connection, "org.freedesktop.login1", Gio.BusNameOwnerFlags.NONE, None, None
    )
    connection.call_sync(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "GetNameOwner",
        GLib.Variant("(s)", ("org.freedesktop.login1",)),
        None,
        Gio.DBusCallFlags.NONE,
        -1,
        None,
    )
    apply_word("allow")
    write_count()
    with open(READY, "w", encoding="utf-8") as ready:
        ready.write("ready\n")
    GLib.timeout_add(50, watch_control)
    GLib.MainLoop().run()


if __name__ == "__main__":
    sys.exit(main())
