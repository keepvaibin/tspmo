// tspmod over D-Bus: state from PropertiesChanged (no polling), changes as method calls.
import QtQuick
import org.kde.plasma.workspace.dbus as DBus

QtObject {
    id: daemon

    // The system bus in production; the session bus for the mock daemon (tests).
    property var busType: DBus.BusType.System

    readonly property bool online: watcher.registered
    readonly property string profile: props.properties.Profile ?? ""
    readonly property string gpuPolicy: props.properties.GpuPolicy ?? ""
    readonly property string gpuMode: props.properties.GpuMode ?? ""
    readonly property string gpuSwitching: props.properties.GpuSwitching ?? ""
    readonly property bool onBattery: props.properties.OnBattery ?? false
    readonly property bool dgpuAsleep: props.properties.DgpuAsleep ?? false
    // Last failure, one short line; cleared by the next call.
    property string error: ""

    readonly property var props: DBus.Properties {
        busType: daemon.busType
        service: "org.tspmo.Daemon1"
        path: "/org/tspmo"
        iface: "org.tspmo.Daemon1"
    }

    readonly property var watcher: DBus.DBusServiceWatcher {
        busType: daemon.busType
        watchedService: "org.tspmo.Daemon1"
    }

    function call(member, argument) {
        daemon.error = "";
        const bus = daemon.busType === DBus.BusType.Session ? DBus.SessionBus : DBus.SystemBus;
        bus.asyncCall({
            service: "org.tspmo.Daemon1",
            path: "/org/tspmo",
            iface: "org.tspmo.Daemon1",
            member: member,
            arguments: [argument],
        }, () => {}, (err) => {
            daemon.error = err.message;
        });
    }

    function setProfile(mode) { call("SetProfile", mode); }
    function setGpu(mode) { call("SetGpuMode", mode); }
}
