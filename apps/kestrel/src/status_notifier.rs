use crate::ApplicationCommand;
use async_channel::Sender;
use kestrel_core::{CapabilityReport, CapabilityStatus};
use ksni::{
    Category, MenuItem, Status, ToolTip, Tray,
    blocking::{Handle, TrayMethods},
    menu::StandardItem,
};

pub const FEATURE_ID: &str = "app.status-notifier";

pub struct StatusNotifierIntegration {
    handle: Option<Handle<KestrelTray>>,
    capability: CapabilityReport,
}

impl StatusNotifierIntegration {
    pub fn start(sender: Sender<ApplicationCommand>) -> Self {
        let tray = KestrelTray { sender };
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
}

impl KestrelTray {
    fn send(&self, command: ApplicationCommand) {
        let _ = self.sender.try_send(command);
    }
}

impl Tray for KestrelTray {
    fn id(&self) -> String {
        "kestrel".to_owned()
    }

    fn category(&self) -> Category {
        Category::SystemServices
    }

    fn title(&self) -> String {
        "Kestrel".to_owned()
    }

    fn status(&self) -> Status {
        Status::Active
    }

    fn icon_name(&self) -> String {
        "io.github.kamleshkc2002.Kestrel".to_owned()
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
