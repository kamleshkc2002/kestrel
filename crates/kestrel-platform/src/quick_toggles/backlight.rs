use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use kestrel_core::{CapabilityEvidence, CapabilityReport, CapabilityStatus, Permission};

use super::{
    QuickToggleControl, QuickToggleError, QuickToggleErrorKind, QuickToggleId, QuickToggleMutation,
    QuickToggleObservation, path_name, permission_denied, supported,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceKind {
    Display,
    Keyboard,
}

#[derive(Debug)]
struct BacklightDevice {
    path: PathBuf,
    current: u64,
    maximum: u64,
}

pub(crate) fn capability(id: QuickToggleId, root: &Path, kind: DeviceKind) -> CapabilityReport {
    let device = match discover(root, kind) {
        Ok(device) => device,
        Err(error) => {
            return CapabilityReport::new(
                id.feature_id(),
                CapabilityStatus::Unsupported {
                    reason: error.message.clone(),
                },
                format!("{} hardware was not detected.", id.label()),
            )
            .with_remediation(match kind {
                DeviceKind::Display => {
                    "Use the display or compositor controls for external and unsupported panels."
                }
                DeviceKind::Keyboard => {
                    "Use the keyboard firmware or desktop control when no sysfs LED is exposed."
                }
            });
        }
    };

    let brightness_path = device.path.join("brightness");
    if let Err(error) = OpenOptions::new().write(true).open(&brightness_path) {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            return permission_denied(
                id,
                Permission::HardwareControl,
                format!(
                    "{} is visible but not writable by the current user.",
                    id.label()
                ),
                "Grant a narrowly scoped udev/logind permission for this backlight; Kestrel will not elevate itself.",
            )
            .with_evidence(CapabilityEvidence::new(
                "device",
                path_name(&device.path),
            ));
        }
        return CapabilityReport::new(
            id.feature_id(),
            CapabilityStatus::Unsupported {
                reason: format!("the brightness control cannot be opened: {error}"),
            },
            format!("{} cannot be controlled.", id.label()),
        )
        .with_remediation(format!(
            "Inspect the detected backlight device and restore current-user access: {error}"
        ));
    }

    supported(
        id,
        format!("{} can be changed for the detected device.", id.label()),
        "Linux sysfs backlight",
    )
    .with_evidence(CapabilityEvidence::new("device", path_name(&device.path)))
}

pub(crate) fn observe(
    root: &Path,
    kind: DeviceKind,
) -> Result<QuickToggleObservation, QuickToggleError> {
    let device = discover(root, kind)?;
    let percentage = percentage(device.current, device.maximum);
    Ok(QuickToggleObservation {
        control: QuickToggleControl::Level { percentage },
        detail: format!("{} is at {percentage}%.", path_name(&device.path)),
    })
}

pub(crate) fn set_level(
    root: &Path,
    kind: DeviceKind,
    mutation: &QuickToggleMutation,
) -> Result<(), QuickToggleError> {
    let QuickToggleMutation::SetLevel(requested) = mutation else {
        return Err(QuickToggleError::new(
            QuickToggleErrorKind::InvalidRequest,
            "this adapter accepts only a percentage level",
        ));
    };
    if *requested > 100 {
        return Err(QuickToggleError::new(
            QuickToggleErrorKind::InvalidRequest,
            format!("brightness {requested}% is outside the supported 0..=100% range"),
        ));
    }

    let device = discover(root, kind)?;
    let minimum = match kind {
        DeviceKind::Display => 1,
        DeviceKind::Keyboard => 0,
    };
    let requested = u64::from(*requested).max(minimum);
    let raw = device.maximum.saturating_mul(requested).saturating_add(50) / 100;
    let raw = raw.clamp(minimum, device.maximum);
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(device.path.join("brightness"))
        .map_err(|error| QuickToggleError::io("opening the brightness control", &error))?;
    write!(file, "{raw}")
        .map_err(|error| QuickToggleError::io("writing the brightness control", &error))
}

fn discover(root: &Path, kind: DeviceKind) -> Result<BacklightDevice, QuickToggleError> {
    let entries = fs::read_dir(root).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            QuickToggleError::unavailable(format!("{} is not exposed", root.display()))
        } else {
            QuickToggleError::io("reading the backlight device directory", &error)
        }
    })?;
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| match kind {
            DeviceKind::Display => true,
            DeviceKind::Keyboard => path_name(path)
                .to_ascii_lowercase()
                .contains("kbd_backlight"),
        })
        .collect::<Vec<_>>();
    paths.sort();

    for path in paths {
        let maximum = read_number(&path.join("max_brightness"))?;
        if maximum == 0 {
            continue;
        }
        let current = read_number(&path.join("brightness"))?;
        return Ok(BacklightDevice {
            path,
            current: current.min(maximum),
            maximum,
        });
    }

    Err(QuickToggleError::unavailable(match kind {
        DeviceKind::Display => "no internal backlight device was detected",
        DeviceKind::Keyboard => "no keyboard backlight device was detected",
    }))
}

fn read_number(path: &Path) -> Result<u64, QuickToggleError> {
    let value = fs::read_to_string(path)
        .map_err(|error| QuickToggleError::io("reading a backlight value", &error))?;
    value.trim().parse().map_err(|_| {
        QuickToggleError::protocol(format!(
            "{} did not contain an unsigned brightness value",
            path.display()
        ))
    })
}

fn percentage(current: u64, maximum: u64) -> u8 {
    current
        .saturating_mul(100)
        .saturating_add(maximum / 2)
        .checked_div(maximum)
        .unwrap_or(0)
        .min(100) as u8
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::{DeviceKind, observe, set_level};
    use crate::quick_toggles::{QuickToggleControl, QuickToggleMutation};

    #[test]
    fn display_brightness_never_writes_a_black_screen_level() {
        let directory = TempDir::new().expect("temporary root");
        let device = directory.path().join("intel_backlight");
        fs::create_dir_all(&device).expect("device directory");
        fs::write(device.join("max_brightness"), "1000").expect("maximum");
        fs::write(device.join("brightness"), "500").expect("brightness");

        set_level(
            directory.path(),
            DeviceKind::Display,
            &QuickToggleMutation::SetLevel(0),
        )
        .expect("level is writable");

        assert_eq!(fs::read_to_string(device.join("brightness")).unwrap(), "10");
    }

    #[test]
    fn keyboard_discovery_ignores_unrelated_leds() {
        let directory = TempDir::new().expect("temporary root");
        let unrelated = directory.path().join("input0::capslock");
        fs::create_dir_all(&unrelated).expect("unrelated LED");
        fs::write(unrelated.join("max_brightness"), "1").expect("maximum");
        fs::write(unrelated.join("brightness"), "1").expect("brightness");
        let keyboard = directory.path().join("platform::kbd_backlight");
        fs::create_dir_all(&keyboard).expect("keyboard LED");
        fs::write(keyboard.join("max_brightness"), "3").expect("maximum");
        fs::write(keyboard.join("brightness"), "1").expect("brightness");

        let observation = observe(directory.path(), DeviceKind::Keyboard).expect("keyboard found");

        assert_eq!(
            observation.control,
            QuickToggleControl::Level { percentage: 33 }
        );
    }
}
