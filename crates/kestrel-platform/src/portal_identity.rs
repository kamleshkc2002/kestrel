//! Naming the app to desktop portals: the host registry call and, on hosts
//! without it, a systemd scope of the app's own.

use std::collections::HashMap;
use std::time::Duration;

use zbus::blocking::connection::Builder;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

pub(crate) const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
pub(crate) const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const REGISTRY_INTERFACE: &str = "org.freedesktop.host.portal.Registry";

const SYSTEMD_BUS: &str = "org.freedesktop.systemd1";
const SYSTEMD_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MANAGER: &str = "org.freedesktop.systemd1.Manager";
/// The manager call must not hold up startup.
const SCOPE_TIMEOUT: Duration = Duration::from_secs(1);

/// Tells the host portal registry which app is asking, before the first
/// request on `connection`. Portals before 1.19 do not export the registry;
/// the call is then skipped without affecting the request that follows.
pub(crate) fn register_application(connection: &Connection, app_id: &str) {
    let registration = Proxy::new(connection, PORTAL_BUS, PORTAL_PATH, REGISTRY_INTERFACE)
        .and_then(|registry| {
            let options: HashMap<String, OwnedValue> = HashMap::new();
            registry.call::<_, _, ()>("Register", &(app_id, options))
        });
    match registration {
        Ok(()) => log::debug!(
            target: "kestrel",
            "registered the app with the host portal registry"
        ),
        Err(error) if registry_unavailable(&error) => log::debug!(
            target: "kestrel",
            "the host portal registry is not available; portals identify the app by its unit"
        ),
        Err(error) => log::debug!(
            target: "kestrel",
            "the host portal registry call failed: {error}"
        ),
    }
}

/// Whether the portal answered that it has no such interface or method.
fn registry_unavailable(error: &zbus::Error) -> bool {
    match error {
        zbus::Error::MethodError(name, ..) => matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.UnknownInterface"
                | "org.freedesktop.DBus.Error.UnknownMethod"
        ),
        _ => false,
    }
}

/// Moves this process into `app-<app_id>-<random>.scope` when it inherits a
/// terminal or file-manager unit, so portals can identify the app. A sandboxed
/// build already carries its identity. Failures only leave the current unit.
pub fn adopt_app_scope(app_id: &str) {
    if crate::sandbox::flatpak() {
        return;
    }
    let Ok(cgroup) = std::fs::read_to_string("/proc/self/cgroup") else {
        return;
    };
    if is_own_app_unit(&cgroup, app_id) {
        return;
    }
    match start_app_scope(app_id) {
        Ok(()) => log::debug!(target: "kestrel", "started the app's own systemd scope"),
        Err(error) => log::debug!(
            target: "kestrel",
            "the app's own systemd scope could not be started: {error}"
        ),
    }
}

/// Whether a `/proc/self/cgroup` file already names this app's own unit.
fn is_own_app_unit(cgroup: &str, app_id: &str) -> bool {
    let Some(unit) = cgroup_v2_unit(cgroup) else {
        return false;
    };
    app_unit_matches(unit, app_id)
}

/// The cgroup v2 path's last component.
fn cgroup_v2_unit(cgroup: &str) -> Option<&str> {
    let path = cgroup.lines().find_map(|line| line.strip_prefix("0::"))?;
    path.rsplit('/').find(|component| !component.is_empty())
}

/// Matches the XDG desktop unit names for an app ID:
/// `app-[<launcher>-]<app-id>-<random>.scope` and
/// `app-[<launcher>-]<app-id>[@<random>].service`.
fn app_unit_matches(unit: &str, app_id: &str) -> bool {
    let Some(rest) = unit.strip_prefix("app-") else {
        return false;
    };
    if let Some(body) = rest.strip_suffix(".scope") {
        let Some((prefix, random)) = body.rsplit_once('-') else {
            return false;
        };
        return !random.is_empty() && !random.contains('-') && app_prefix_matches(prefix, app_id);
    }
    let Some(body) = rest.strip_suffix(".service") else {
        return false;
    };
    let Some((prefix, instance)) = body.split_once('@') else {
        return app_prefix_matches(body, app_id);
    };
    !instance.contains('@') && app_prefix_matches(prefix, app_id)
}

/// An app ID, optionally after the launcher token of a desktop launch.
fn app_prefix_matches(prefix: &str, app_id: &str) -> bool {
    prefix == app_id
        || prefix
            .strip_suffix(app_id)
            .is_some_and(|launcher| launcher.len() > 1 && launcher.ends_with('-'))
}

fn start_app_scope(app_id: &str) -> Result<(), zbus::Error> {
    let connection = Builder::session()?.method_timeout(SCOPE_TIMEOUT).build()?;
    let manager = Proxy::new(&connection, SYSTEMD_BUS, SYSTEMD_PATH, SYSTEMD_MANAGER)?;
    let properties: Vec<(&str, Value<'static>)> = vec![
        ("PIDs", Value::from(vec![std::process::id()])),
        ("CollectMode", Value::from("inactive-or-failed")),
    ];
    let auxiliary: Vec<(&str, Vec<(&str, Value<'static>)>)> = Vec::new();
    let _: OwnedObjectPath = manager.call(
        "StartTransientUnit",
        &(
            app_scope_name(app_id).as_str(),
            "fail",
            properties,
            auxiliary,
        ),
    )?;
    Ok(())
}

/// `app-<app-id>-<pid><random hex>.scope`; the part after the last dash has
/// no dash of its own.
fn app_scope_name(app_id: &str) -> String {
    let mut random = [0u8; 4];
    let _ = getrandom::fill(&mut random);
    format!(
        "app-{app_id}-{}{:08x}.scope",
        std::process::id(),
        u32::from_ne_bytes(random)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP_ID: &str = "io.github.kamleshkc2002.Kestrel";
    const DEVEL_ID: &str = "io.github.kamleshkc2002.Kestrel.Devel";

    fn cgroup(unit: &str) -> String {
        format!("0::/user.slice/user-1000.slice/user@1000.service/app.slice/{unit}\n")
    }

    #[test]
    fn recognises_only_the_apps_own_units() {
        for (name, content, app_id, expected) in [
            (
                "a terminal's scope",
                cgroup("app-gnome-org.gnome.Terminal-1234.scope"),
                APP_ID,
                false,
            ),
            (
                "a file manager's scope",
                cgroup("app-org.gnome.Nautilus-4321.scope"),
                APP_ID,
                false,
            ),
            (
                "the app's own scope",
                cgroup("app-io.github.kamleshkc2002.Kestrel-9384.scope"),
                APP_ID,
                true,
            ),
            (
                "the app's own service",
                cgroup("app-io.github.kamleshkc2002.Kestrel@a1b2c3.service"),
                APP_ID,
                true,
            ),
            (
                "an instance-less app service",
                cgroup("app-io.github.kamleshkc2002.Kestrel.service"),
                APP_ID,
                true,
            ),
            (
                "a launcher-prefixed scope",
                cgroup("app-gnome-io.github.kamleshkc2002.Kestrel-123.scope"),
                APP_ID,
                true,
            ),
            (
                "the development scope",
                cgroup("app-io.github.kamleshkc2002.Kestrel.Devel-55.scope"),
                DEVEL_ID,
                true,
            ),
            (
                "the release app in the development scope",
                cgroup("app-io.github.kamleshkc2002.Kestrel.Devel-55.scope"),
                APP_ID,
                false,
            ),
            ("a session scope", cgroup("session-3.scope"), APP_ID, false),
            (
                "a malformed line",
                "not a cgroup line\n".to_owned(),
                APP_ID,
                false,
            ),
            ("an empty v2 path", "0::\n".to_owned(), APP_ID, false),
        ] {
            assert_eq!(is_own_app_unit(&content, app_id), expected, "{name}");
        }
    }

    #[test]
    fn scope_names_carry_the_app_id_and_a_dash_free_random_part() {
        let name = app_scope_name(APP_ID);
        assert!(name.starts_with("app-io.github.kamleshkc2002.Kestrel-"));
        let random = name.rsplit_once('-').expect("the unit has a random part").1;
        assert!(!random.is_empty());
        assert!(!random.contains('-'));
        assert!(random.starts_with(&std::process::id().to_string()));
        assert!(is_own_app_unit(&cgroup(&name), APP_ID));
    }
}
