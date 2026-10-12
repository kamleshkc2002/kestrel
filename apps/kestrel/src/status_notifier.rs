use crate::{ApplicationCommand, tray_icon::readout_icon};
use async_channel::Sender;
use kestrel_core::{CapabilityReport, CapabilityStatus};
use ksni::{
    Category, Icon, MenuItem, Status, ToolTip, Tray,
    blocking::{Handle, TrayMethods},
    menu::StandardItem,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub const FEATURE_ID: &str = "app.status-notifier";
const APPLICATION_ICON: &str = "io.github.kamleshkc2002.Kestrel";

pub struct StatusNotifierIntegration {
    handle: Option<Handle<KestrelTray>>,
    sender: Sender<ApplicationCommand>,
    capability: CapabilityReport,
}

impl StatusNotifierIntegration {
    pub fn start(sender: Sender<ApplicationCommand>) -> Self {
        let tray = KestrelTray {
            sender: sender.clone(),
        };
        match tray.spawn() {
            Ok(handle) => Self {
                handle: Some(handle),
                sender,
                capability: CapabilityReport::new(
                    FEATURE_ID,
                    CapabilityStatus::Supported,
                    "The desktop tray host accepted Kestrel's optional StatusNotifierItem.",
                )
                .with_selected_backend("StatusNotifierItem over the user session D-Bus"),
            },
            Err(error) => Self {
                handle: None,
                sender,
                capability: unavailable_capability(format!(
                    "StatusNotifierItem registration failed: {error}"
                )),
            },
        }
    }

    pub fn capability(&self) -> CapabilityReport {
        self.capability.clone()
    }

    /// Publishes monitor readouts as tray items; `None` without a tray host.
    pub fn readout_sink(&self) -> Option<TrayReadoutSink> {
        self.handle.as_ref().map(|_| TrayReadoutSink {
            sender: self.sender.clone(),
            items: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }
}

impl Drop for StatusNotifierIntegration {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown().wait();
        }
    }
}

/// One monitor readout shown as its own tray item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayReadout {
    /// Stable per readout; it names the item for hosts that remember placement.
    pub key: String,
    pub label: &'static str,
    /// Few letters for the icon, such as "MEM".
    pub icon_label: &'static str,
    /// Full reading for the title, tooltip, and menu, such as "1.6 Mbit/s".
    pub value: String,
    /// Icon text short enough for a square panel icon, such as "1.6Mb".
    pub compact: String,
    pub detail: String,
}

/// Keeps one tray item per configured readout: adds, updates, and removes
/// items as the configuration and readings change.
#[derive(Clone)]
pub struct TrayReadoutSink {
    sender: Sender<ApplicationCommand>,
    items: Arc<Mutex<BTreeMap<String, ReadoutItem>>>,
}

struct ReadoutItem {
    handle: Handle<ReadoutTray>,
    readout: TrayReadout,
}

impl TrayReadoutSink {
    /// Blocks briefly on each tray thread; call it off the GTK thread.
    pub fn publish(&self, readouts: &[TrayReadout]) {
        let mut items = self
            .items
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        items.retain(|key, item| {
            let keep = readouts.iter().any(|readout| &readout.key == key);
            if !keep {
                item.handle.shutdown().wait();
            }
            keep && !item.handle.is_closed()
        });
        for readout in readouts {
            match items.get_mut(&readout.key) {
                Some(item) if item.readout == *readout => {}
                Some(item) => {
                    let icon = (item.readout.compact != readout.compact
                        || item.readout.icon_label != readout.icon_label)
                        .then(|| icon_for(readout));
                    let applied = readout.clone();
                    item.handle.update(move |tray| {
                        tray.readout = applied;
                        if let Some(icon) = icon {
                            tray.icon = icon;
                        }
                    });
                    item.readout = readout.clone();
                }
                None => {
                    let tray = ReadoutTray {
                        sender: self.sender.clone(),
                        icon: icon_for(readout),
                        readout: readout.clone(),
                    };
                    // A host that rejects the item leaves the readout in the window only.
                    if let Ok(handle) = tray.spawn() {
                        items.insert(
                            readout.key.clone(),
                            ReadoutItem {
                                handle,
                                readout: readout.clone(),
                            },
                        );
                    }
                }
            }
        }
    }
}

fn icon_for(readout: &TrayReadout) -> Vec<Icon> {
    readout_icon(readout.icon_label, &readout.compact)
        .map(|pixels| Icon {
            width: pixels.width,
            height: pixels.height,
            data: pixels.argb,
        })
        .into_iter()
        .collect()
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

fn send(sender: &Sender<ApplicationCommand>, command: ApplicationCommand) {
    let _ = sender.try_send(command);
}

fn open_item<T: Tray>(sender: &Sender<ApplicationCommand>) -> MenuItem<T> {
    let sender = sender.clone();
    StandardItem {
        label: "Open Kestrel".to_owned(),
        icon_name: "window-new-symbolic".to_owned(),
        activate: Box::new(move |_: &mut T| send(&sender, ApplicationCommand::PresentWindow)),
        ..StandardItem::default()
    }
    .into()
}

#[derive(Debug)]
struct KestrelTray {
    sender: Sender<ApplicationCommand>,
}

impl Tray for KestrelTray {
    fn id(&self) -> String {
        kestrel_core::DATA_DIRECTORY_NAME.to_owned()
    }

    fn category(&self) -> Category {
        Category::SystemServices
    }

    fn title(&self) -> String {
        kestrel_core::APPLICATION_NAME.to_owned()
    }

    fn status(&self) -> Status {
        Status::Active
    }

    fn icon_name(&self) -> String {
        APPLICATION_ICON.to_owned()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: self.icon_name(),
            title: self.title(),
            description: "Open Kestrel's capability and feature status window.".to_owned(),
            ..ToolTip::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        send(&self.sender, ApplicationCommand::PresentWindow);
    }

    /// Wayland hosts send this before an activation so the window may come forward.
    fn provide_xdg_activation_token(&mut self, token: String) {
        send(&self.sender, ApplicationCommand::ActivationToken(token));
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            open_item(&self.sender),
            StandardItem {
                label: "Refresh capabilities".to_owned(),
                icon_name: "view-refresh-symbolic".to_owned(),
                activate: Box::new(|tray: &mut Self| {
                    send(&tray.sender, ApplicationCommand::RefreshCapabilities)
                }),
                ..StandardItem::default()
            }
            .into(),
            StandardItem {
                label: "Toggle microphone mute".to_owned(),
                icon_name: "audio-input-microphone-symbolic".to_owned(),
                activate: Box::new(|tray: &mut Self| {
                    send(
                        &tray.sender,
                        ApplicationCommand::Microphone(
                            kestrel_services::microphone::MicrophoneCommand::ToggleMute,
                        ),
                    )
                }),
                ..StandardItem::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".to_owned(),
                icon_name: "application-exit-symbolic".to_owned(),
                activate: Box::new(|tray: &mut Self| send(&tray.sender, ApplicationCommand::Quit)),
                ..StandardItem::default()
            }
            .into(),
        ]
    }
}

/// A tray item whose icon shows one readout's label and value.
#[derive(Debug)]
struct ReadoutTray {
    sender: Sender<ApplicationCommand>,
    readout: TrayReadout,
    icon: Vec<Icon>,
}

impl ReadoutTray {
    fn reading(&self) -> String {
        format!("{} {}", self.readout.label, self.readout.value)
    }
}

impl Tray for ReadoutTray {
    fn id(&self) -> String {
        format!("{}-{}", kestrel_core::DATA_DIRECTORY_NAME, self.readout.key)
    }

    fn category(&self) -> Category {
        Category::Hardware
    }

    fn title(&self) -> String {
        self.reading()
    }

    fn status(&self) -> Status {
        Status::Active
    }

    /// Hosts prefer a named icon, so none is set and the readout pixmap shows.
    fn icon_name(&self) -> String {
        String::new()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icon.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: APPLICATION_ICON.to_owned(),
            title: self.reading(),
            description: self.readout.detail.clone(),
            ..ToolTip::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        send(&self.sender, ApplicationCommand::PresentWindow);
    }

    fn provide_xdg_activation_token(&mut self, token: String) {
        send(&self.sender, ApplicationCommand::ActivationToken(token));
    }

    /// Hosts without tooltips still show the full reading here.
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: self.reading(),
                enabled: false,
                ..StandardItem::default()
            }
            .into(),
        ];
        if !self.readout.detail.is_empty() {
            items.push(
                StandardItem {
                    label: self.readout.detail.clone(),
                    enabled: false,
                    ..StandardItem::default()
                }
                .into(),
            );
        }
        items.push(MenuItem::Separator);
        items.push(open_item(&self.sender));
        items
    }
}

#[cfg(test)]
mod tests {
    use kestrel_core::CapabilityStatus;

    use super::{FEATURE_ID, unavailable_capability};

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
