//! Whether the session has a tray host for StatusNotifierItem icons, found
//! without registering an item.

use zbus::{blocking::Connection, blocking::fdo::DBusProxy, names::BusName};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";

/// `Ok(true)` while a StatusNotifierWatcher owns its name on the session bus.
pub fn tray_host_present() -> Result<bool, String> {
    let connection = Connection::session()
        .map_err(|error| format!("connecting to the session bus failed: {error}"))?;
    let proxy = DBusProxy::new(&connection)
        .map_err(|error| format!("reaching the session bus failed: {error}"))?;
    let watcher =
        BusName::try_from(WATCHER).map_err(|error| format!("invalid watcher name: {error}"))?;
    proxy
        .name_has_owner(watcher)
        .map_err(|error| format!("asking for the tray host failed: {error}"))
}
