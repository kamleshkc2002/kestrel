use crate::ApplicationCommand;
use async_channel::Sender;
use kestrel_core::{CapabilityReport, CapabilityStatus};
use ksni::{
    Category, Icon, MenuItem, Status, ToolTip, Tray,
    blocking::{Handle, TrayMethods},
    menu::StandardItem,
};
use std::sync::{Arc, Mutex};

pub const FEATURE_ID: &str = "app.status-notifier";

pub struct StatusNotifierIntegration {
    handle: Option<Handle<KestrelTray>>,
    capability: CapabilityReport,
}

impl StatusNotifierIntegration {
    pub fn start(sender: Sender<ApplicationCommand>) -> Self {
        let tray = KestrelTray {
            sender,
            readouts: None,
        };
        match tray.spawn() {
            Ok(handle) => Self {
                handle: Some(handle),
                capability: CapabilityReport::new(
                    FEATURE_ID,
                    CapabilityStatus::Supported,
                    "The desktop tray host accepted Kestrel's optional StatusNotifierItem.",
                )
                .with_selected_backend("StatusNotifierItem over the user session D-Bus"),
            },
            Err(error) => Self {
                handle: None,
                capability: unavailable_capability(format!(
                    "StatusNotifierItem registration failed: {error}"
                )),
            },
        }
    }

    pub fn capability(&self) -> CapabilityReport {
        self.capability.clone()
    }

    /// Publishes monitor readouts to the tray; `None` without a tray host.
    pub fn readout_sink(&self) -> Option<TrayReadoutSink> {
        self.handle.clone().map(|handle| TrayReadoutSink {
            handle,
            last: Arc::new(Mutex::new(None)),
        })
    }
}

/// Monitor readouts shown in the tray title, tooltip, and icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayReadouts {
    /// Bounded aggregate readings such as "CPU 12% · Memory 48%".
    pub summary: String,
    /// Fill level of the meter icon, from the first percentage readout.
    pub meter_percent: Option<u8>,
}

/// Sends readouts to the tray service thread; unchanged values are skipped.
#[derive(Clone)]
pub struct TrayReadoutSink {
    handle: Handle<KestrelTray>,
    last: Arc<Mutex<Option<Option<TrayReadouts>>>>,
}

impl TrayReadoutSink {
    /// Blocks briefly on the tray thread; call it off the GTK thread.
    pub fn publish(&self, readouts: Option<TrayReadouts>) {
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if last.as_ref() == Some(&readouts) {
            return;
        }
        if self.handle.is_closed() {
            return;
        }
        let applied = readouts.clone();
        self.handle.update(move |tray| tray.readouts = applied);
        *last = Some(readouts);
    }
}

impl Drop for StatusNotifierIntegration {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown().wait();
        }
    }
}

pub(crate) fn unavailable_capability(reason: impl Into<String>) -> CapabilityReport {
    CapabilityReport::new(
        FEATURE_ID,
        CapabilityStatus::Unsupported {
            reason: reason.into(),
        },
        "The optional tray entry is unavailable; the normal Kestrel window remains fully usable.",
    )
    .with_remediation(
        "Use the normal window. On GNOME, enable an AppIndicator/StatusNotifierItem extension; on other desktops, start an SNI-compatible panel or tray host, then restart Kestrel.",
    )
}

#[derive(Debug)]
struct KestrelTray {
    sender: Sender<ApplicationCommand>,
    readouts: Option<TrayReadouts>,
}

impl KestrelTray {
    fn send(&self, command: ApplicationCommand) {
        let _ = self.sender.try_send(command);
    }
}

const METER_SIZE: i32 = 22;

/// A vertical meter in network-order ARGB32: an outlined bar filled from the bottom.
fn meter_icon(percent: u8) -> Icon {
    let size = METER_SIZE as usize;
    let (left, right, top, bottom) = (5, size - 5, 1, size - 1);
    let inner_height = bottom - top - 2;
    let filled = (usize::from(percent.min(100)) * inner_height).div_ceil(100);
    let mut data = vec![0_u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let inside = (left..right).contains(&x) && (top..bottom).contains(&y);
            if !inside {
                continue;
            }
            let border = x == left || x == right - 1 || y == top || y == bottom - 1;
            let fill = y >= bottom - 1 - filled;
            let argb: [u8; 4] = if border {
                [0xff, 0xe8, 0xe8, 0xe8]
            } else if fill {
                match percent {
                    90.. => [0xff, 0xe0, 0x4b, 0x3c],
                    70..=89 => [0xff, 0xf0, 0xb0, 0x30],
                    _ => [0xff, 0x4c, 0xb0, 0x5a],
                }
            } else {
                [0x80, 0x20, 0x20, 0x20]
            };
            let offset = (y * size + x) * 4;
            data[offset..offset + 4].copy_from_slice(&argb);
        }
    }
    Icon {
        width: METER_SIZE,
        height: METER_SIZE,
        data,
    }
}

impl Tray for KestrelTray {
    fn id(&self) -> String {
        kestrel_core::DATA_DIRECTORY_NAME.to_owned()
    }

    fn category(&self) -> Category {
        Category::SystemServices
    }

    fn title(&self) -> String {
        match &self.readouts {
            Some(readouts) => format!("{} · {}", kestrel_core::APPLICATION_NAME, readouts.summary),
            None => kestrel_core::APPLICATION_NAME.to_owned(),
        }
    }

    fn status(&self) -> Status {
        Status::Active
    }

    /// Hosts prefer a named icon, so the meter replaces it while one is set.
    fn icon_name(&self) -> String {
        match self
            .readouts
            .as_ref()
            .and_then(|readouts| readouts.meter_percent)
        {
            Some(_) => String::new(),
            None => "io.github.kamleshkc2002.Kestrel".to_owned(),
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.readouts
            .as_ref()
            .and_then(|readouts| readouts.meter_percent)
            .map(meter_icon)
            .into_iter()
            .collect()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: "io.github.kamleshkc2002.Kestrel".to_owned(),
            title: kestrel_core::APPLICATION_NAME.to_owned(),
            description: match &self.readouts {
                Some(readouts) => readouts.summary.clone(),
                None => "Open Kestrel's capability and feature status window.".to_owned(),
            },
            ..ToolTip::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(ApplicationCommand::PresentWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Open Kestrel".to_owned(),
                icon_name: "window-new-symbolic".to_owned(),
                activate: Box::new(|tray: &mut Self| tray.send(ApplicationCommand::PresentWindow)),
                ..StandardItem::default()
            }
            .into(),
            StandardItem {
                label: "Refresh capabilities".to_owned(),
                icon_name: "view-refresh-symbolic".to_owned(),
                activate: Box::new(|tray: &mut Self| {
                    tray.send(ApplicationCommand::RefreshCapabilities)
                }),
                ..StandardItem::default()
            }
            .into(),
            StandardItem {
                label: "Toggle microphone mute".to_owned(),
                icon_name: "audio-input-microphone-symbolic".to_owned(),
                activate: Box::new(|tray: &mut Self| {
                    tray.send(ApplicationCommand::Microphone(
                        kestrel_services::microphone::MicrophoneCommand::ToggleMute,
                    ))
                }),
                ..StandardItem::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".to_owned(),
                icon_name: "application-exit-symbolic".to_owned(),
                activate: Box::new(|tray: &mut Self| tray.send(ApplicationCommand::Quit)),
                ..StandardItem::default()
            }
            .into(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use kestrel_core::CapabilityStatus;

    use super::{FEATURE_ID, meter_icon, unavailable_capability};

    #[test]
    fn meter_fill_follows_the_percentage() {
        let filled_pixels = |percent| {
            let icon = meter_icon(percent);
            assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
            // Green, amber, or red fill pixels; the border and track differ.
            icon.data
                .chunks(4)
                .filter(|pixel| {
                    matches!(
                        pixel,
                        [0xff, 0x4c, 0xb0, 0x5a]
                            | [0xff, 0xf0, 0xb0, 0x30]
                            | [0xff, 0xe0, 0x4b, 0x3c]
                    )
                })
                .count()
        };
        assert_eq!(filled_pixels(0), 0);
        assert!(filled_pixels(30) < filled_pixels(60));
        assert!(filled_pixels(60) < filled_pixels(100));
        assert_eq!(
            filled_pixels(100),
            filled_pixels(255),
            "values above 100 are capped"
        );
    }

    #[test]
    fn unavailable_tray_keeps_the_normal_window_as_remediation() {
        let report = unavailable_capability("No tray host is available.");

        assert_eq!(report.feature_id, FEATURE_ID);
        assert!(matches!(
            report.status,
            CapabilityStatus::Unsupported { .. }
        ));
        assert!(report.summary.contains("normal Kestrel window"));
        assert!(
            report
                .remediation
                .as_deref()
                .is_some_and(|message| message.contains("GNOME"))
        );
    }
}
