use std::{collections::HashMap, sync::Mutex};

use zbus::{
    blocking::{Connection, Proxy},
    zvariant::{OwnedObjectPath, OwnedValue},
};

const UPOWER_DESTINATION: &str = "org.freedesktop.UPower";
const UPOWER_PATH: &str = "/org/freedesktop/UPower";
const UPOWER_INTERFACE: &str = "org.freedesktop.UPower";
const DEVICE_INTERFACE: &str = "org.freedesktop.UPower.Device";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
const DISPLAY_DEVICE_PATH: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

pub const MAX_PERIPHERALS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeripheralKind {
    Mouse,
    Keyboard,
    Headset,
    Headphones,
    Speakers,
    GamingInput,
    Pen,
    Touchpad,
    Phone,
    Tablet,
    Other,
}

impl PeripheralKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mouse => "Mouse",
            Self::Keyboard => "Keyboard",
            Self::Headset => "Headset",
            Self::Headphones => "Headphones",
            Self::Speakers => "Speakers",
            Self::GamingInput => "Game controller",
            Self::Pen => "Pen",
            Self::Touchpad => "Touchpad",
            Self::Phone => "Phone",
            Self::Tablet => "Tablet",
            Self::Other => "Device",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeripheralCharge {
    Charging,
    Discharging,
    Full,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeripheralBattery {
    /// Stable short identity derived from UPower's native path, never a serial.
    pub id: String,
    pub kind: PeripheralKind,
    pub model: Option<String>,
    pub percentage: Option<u8>,
    pub charge: PeripheralCharge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeripheralIssue {
    pub reason: String,
    pub remediation: String,
}

/// Builds a peripheral from one UPower device property map.
///
/// State 5 (pending charge) is presented as charging because the device is
/// waiting to begin charging.
pub fn peripheral_from_properties(
    object_path: &str,
    properties: &HashMap<String, OwnedValue>,
) -> Option<PeripheralBattery> {
    if object_path == DISPLAY_DEVICE_PATH
        || property_bool(properties, "PowerSupply")
        || properties
            .get("IsPresent")
            .and_then(|value| bool::try_from(value).ok())
            == Some(false)
    {
        return None;
    }

    let device_type = property_u32(properties, "Type")?;
    if device_type == 1 {
        return None;
    }

    let kind = match device_type {
        5 => PeripheralKind::Mouse,
        6 => PeripheralKind::Keyboard,
        8 => PeripheralKind::Phone,
        10 => PeripheralKind::Tablet,
        12 => PeripheralKind::GamingInput,
        13 => PeripheralKind::Pen,
        14 => PeripheralKind::Touchpad,
        17 => PeripheralKind::Headset,
        18 => PeripheralKind::Speakers,
        19 => PeripheralKind::Headphones,
        _ => PeripheralKind::Other,
    };

    let native_path = property_string(properties, "NativePath")
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| object_path.to_owned());
    let id = format!("{:016x}", fnv1a(native_path.as_bytes()));
    let model = property_string(properties, "Model").and_then(|model| trim_model(&model));
    let percentage = property_f64(properties, "Percentage").and_then(|percentage| {
        if !percentage.is_finite() {
            return None;
        }
        let rounded = percentage.clamp(0.0, 100.0).round() as u8;
        let coarse_level = property_u32(properties, "BatteryLevel")
            .map(|level| (1..=6).contains(&level))
            .unwrap_or(false);
        if rounded == 0 && coarse_level {
            None
        } else {
            Some(rounded)
        }
    });
    let charge = match property_u32(properties, "State") {
        Some(1 | 5) => PeripheralCharge::Charging,
        Some(2) => PeripheralCharge::Discharging,
        Some(4) => PeripheralCharge::Full,
        _ => PeripheralCharge::Unknown,
    };

    Some(PeripheralBattery {
        id,
        kind,
        model,
        percentage,
        charge,
    })
}

/// A cached system-bus reader for UPower peripheral devices.
pub struct UPowerPeripherals {
    connection: Mutex<Option<Connection>>,
}

impl Default for UPowerPeripherals {
    fn default() -> Self {
        Self {
            connection: Mutex::new(None),
        }
    }
}

impl UPowerPeripherals {
    pub fn read(&self) -> Result<Vec<PeripheralBattery>, PeripheralIssue> {
        let mut slot = self
            .connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_none() {
            *slot = Some(Connection::system().map_err(|error| issue_for_error(&error))?);
        }
        let Some(connection) = slot.as_ref() else {
            return Err(unavailable_issue());
        };

        let manager = match Proxy::new(
            connection,
            UPOWER_DESTINATION,
            UPOWER_PATH,
            UPOWER_INTERFACE,
        ) {
            Ok(proxy) => proxy,
            Err(error) => {
                *slot = None;
                return Err(issue_for_error(&error));
            }
        };
        let paths: Vec<OwnedObjectPath> = match manager.call("EnumerateDevices", &()) {
            Ok(paths) => paths,
            Err(error) => {
                *slot = None;
                return Err(issue_for_error(&error));
            }
        };

        let mut peripherals = Vec::new();
        for path in paths {
            let proxy = match Proxy::new(
                connection,
                UPOWER_DESTINATION,
                path.as_str(),
                PROPERTIES_INTERFACE,
            ) {
                Ok(proxy) => proxy,
                Err(_) => continue,
            };
            let properties: HashMap<String, OwnedValue> =
                match proxy.call("GetAll", &(DEVICE_INTERFACE,)) {
                    Ok(properties) => properties,
                    Err(_) => continue,
                };
            if let Some(peripheral) = peripheral_from_properties(path.as_str(), &properties) {
                peripherals.push(peripheral);
            }
        }

        peripherals.sort_by(peripheral_order);
        peripherals.truncate(MAX_PERIPHERALS);
        Ok(peripherals)
    }
}

fn peripheral_order(left: &PeripheralBattery, right: &PeripheralBattery) -> std::cmp::Ordering {
    left.kind
        .label()
        .cmp(right.kind.label())
        .then_with(|| {
            left.model
                .as_deref()
                .unwrap_or("")
                .cmp(right.model.as_deref().unwrap_or(""))
        })
        .then_with(|| left.id.cmp(&right.id))
}

fn property_bool(properties: &HashMap<String, OwnedValue>, name: &str) -> bool {
    properties
        .get(name)
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

fn property_u32(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<u32> {
    properties
        .get(name)
        .and_then(|value| u32::try_from(value).ok())
}

fn property_f64(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<f64> {
    properties
        .get(name)
        .and_then(|value| f64::try_from(value).ok())
}

fn property_string(properties: &HashMap<String, OwnedValue>, name: &str) -> Option<String> {
    properties
        .get(name)
        .and_then(|value| String::try_from(value.clone()).ok())
}

fn trim_model(model: &str) -> Option<String> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    let mut end = 0;
    for (index, character) in model.char_indices() {
        let next = index + character.len_utf8();
        if next > 64 {
            break;
        }
        end = next;
    }
    Some(model[..end].to_owned())
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn unavailable_issue() -> PeripheralIssue {
    PeripheralIssue {
        reason: "UPower is not available on the system bus".to_owned(),
        remediation: "Install and start the upower service to list accessory batteries.".to_owned(),
    }
}

fn issue_for_error(error: &zbus::Error) -> PeripheralIssue {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("accessdenied")
        || message.contains("permission denied")
        || message.contains("not authorized")
    {
        PeripheralIssue {
            reason: "Access to UPower was denied".to_owned(),
            remediation: "Grant permission to query UPower devices on the system bus.".to_owned(),
        }
    } else {
        unavailable_issue()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Str;

    fn properties(
        entries: impl IntoIterator<Item = (&'static str, OwnedValue)>,
    ) -> HashMap<String, OwnedValue> {
        entries
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect()
    }

    fn mouse_properties() -> HashMap<String, OwnedValue> {
        properties([
            ("Type", OwnedValue::from(5u32)),
            (
                "NativePath",
                OwnedValue::from(Str::from("/sys/devices/mouse")),
            ),
            ("Model", OwnedValue::from(Str::from("Travel Mouse"))),
            ("Percentage", OwnedValue::from(55.4f64)),
            ("State", OwnedValue::from(2u32)),
            ("IsPresent", OwnedValue::from(true)),
        ])
    }

    #[test]
    fn mouse_percentage_is_rounded_and_kind_is_mapped() {
        let mouse = peripheral_from_properties(
            "/org/freedesktop/UPower/devices/mouse",
            &mouse_properties(),
        )
        .expect("mouse is a peripheral");

        assert_eq!(mouse.kind, PeripheralKind::Mouse);
        assert_eq!(mouse.percentage, Some(55));
        assert_eq!(mouse.charge, PeripheralCharge::Discharging);
    }

    #[test]
    fn non_peripheral_devices_are_excluded() {
        for (path, device_type, power_supply, present) in [
            ("/laptop", 2u32, true, true),
            ("/line-power", 1u32, false, true),
            ("/mouse", 5u32, false, false),
        ] {
            let mut values = mouse_properties();
            values.insert("Type".to_owned(), OwnedValue::from(device_type));
            if power_supply {
                values.insert("PowerSupply".to_owned(), OwnedValue::from(true));
            }
            if !present {
                values.insert("IsPresent".to_owned(), OwnedValue::from(false));
            }
            assert!(
                peripheral_from_properties(path, &values).is_none(),
                "{path} is not a peripheral"
            );
        }
    }

    #[test]
    fn model_is_trimmed_and_truncated_at_a_character_boundary() {
        let mut values = mouse_properties();
        let long_model = format!("  {}  ", "é".repeat(40));
        values.insert(
            "Model".to_owned(),
            OwnedValue::from(Str::from(long_model.as_str())),
        );
        let mouse = peripheral_from_properties("/mouse", &values).expect("mouse is a peripheral");
        let model = mouse.model.expect("model is present");
        assert_eq!(model.len(), 64);
        assert!(model.chars().all(|character| character == 'é'));
    }

    #[test]
    fn native_path_stabilizes_identity() {
        let first = peripheral_from_properties("/first", &mouse_properties()).expect("first");
        let second = peripheral_from_properties("/second", &mouse_properties()).expect("second");
        assert_eq!(first.id, second.id);

        let mut values = mouse_properties();
        values.insert(
            "NativePath".to_owned(),
            OwnedValue::from(Str::from("/sys/devices/other-mouse")),
        );
        let third = peripheral_from_properties("/third", &values).expect("third");
        assert_ne!(first.id, third.id);
    }

    #[test]
    fn serial_is_never_in_result_debug_output() {
        let mut values = mouse_properties();
        values.insert(
            "Serial".to_owned(),
            OwnedValue::from(Str::from("private-serial-value")),
        );
        let mouse = peripheral_from_properties("/mouse", &values).expect("mouse");
        assert!(!format!("{mouse:?}").contains("private-serial-value"));
    }
}
