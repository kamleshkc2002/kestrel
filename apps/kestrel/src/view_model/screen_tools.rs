//! Screen tools presentation.

use super::*;

pub(crate) struct ScreenToolGate {
    pub tool: ScreenTool,
    pub running: bool,
    /// Why the tool cannot run; `None` when it is running.
    pub reason: Option<String>,
}

pub(crate) struct ScreenToolsPresentation {
    pub snapshot: ScreenToolsSnapshot,
    pub gates: Vec<ScreenToolGate>,
    pub settings: kestrel_core::ScreenToolsConfiguration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenToolButtonViewModel {
    pub tool: ScreenTool,
    pub label: &'static str,
    pub enabled: bool,
    pub reason: Option<String>,
    pub turn_on: Option<TurnOnViewModel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenToolResultViewModel {
    Text(String),
    Qr(Vec<QrRowViewModel>),
    Color {
        rgb: [u8; 3],
        hex: String,
        css_rgb: String,
        hsl: String,
    },
}

/// One decoded QR code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrRowViewModel {
    pub text: String,
    /// An http(s) address to open.
    pub open: Option<String>,
    /// Text payloads can be copied; binary ones are not shown.
    pub copy: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScreenToolsViewModel {
    pub buttons: Vec<ScreenToolButtonViewModel>,
    /// At least one tool is running.
    pub available: bool,
    pub status: String,
    /// A job is collecting or processing; Cancel is offered.
    pub busy: bool,
    /// The selection view should open for this tool.
    pub selecting: Option<ScreenTool>,
    pub result: Option<ScreenToolResultViewModel>,
    /// Seeds the Settings rows.
    pub settings: kestrel_core::ScreenToolsConfiguration,
}

impl ScreenToolsViewModel {
    pub(crate) fn from_presentation(presentation: ScreenToolsPresentation) -> Self {
        let buttons = presentation
            .gates
            .into_iter()
            .map(|gate| ScreenToolButtonViewModel {
                tool: gate.tool,
                label: gate.tool.label(),
                enabled: gate.running,
                reason: gate.reason,
                turn_on: None,
            })
            .collect::<Vec<_>>();
        let available = buttons.iter().any(|button| button.enabled);
        let phase = presentation.snapshot.phase;
        let busy = matches!(
            phase,
            ScreenToolsPhase::Capturing(_)
                | ScreenToolsPhase::Selecting(_)
                | ScreenToolsPhase::Processing(_)
        );
        let (status, selecting, result) = match phase {
            ScreenToolsPhase::Idle if available => (
                "Pick a tool, then the area or pixel to read. Nothing leaves this computer."
                    .to_owned(),
                None,
                None,
            ),
            ScreenToolsPhase::Idle => (
                "Screen tools are off. Enable capture.text, capture.qr, or capture.color in the \
                 Feature Hub."
                    .to_owned(),
                None,
                None,
            ),
            ScreenToolsPhase::Capturing(tool) => (
                format!("{}: waiting for the screen…", tool.label()),
                None,
                None,
            ),
            ScreenToolsPhase::Selecting(tool) => (
                format!("{}: choose in the selection window.", tool.label()),
                Some(tool),
                None,
            ),
            ScreenToolsPhase::Processing(tool) => {
                (format!("{}: reading…", tool.label()), None, None)
            }
            ScreenToolsPhase::Done(tool, result) => {
                let (status, result) = screen_tool_result(result);
                (format!("{}: {status}", tool.label()), None, Some(result))
            }
            ScreenToolsPhase::Failed(tool, error) => {
                (format!("{}: {}", tool.label(), error.message), None, None)
            }
        };
        Self {
            buttons,
            available,
            status,
            busy,
            selecting,
            result,
            settings: presentation.settings,
        }
    }
}

fn screen_tool_result(result: ScreenToolResult) -> (String, ScreenToolResultViewModel) {
    match result {
        ScreenToolResult::Text(text) if text.trim().is_empty() => (
            "no text was found.".to_owned(),
            ScreenToolResultViewModel::Text(text),
        ),
        ScreenToolResult::Text(text) => (
            format!("{} characters.", text.chars().count()),
            ScreenToolResultViewModel::Text(text),
        ),
        ScreenToolResult::Qr(payloads) => {
            let rows = payloads
                .into_iter()
                .map(|payload| match payload {
                    QrPayload::Text(text) => {
                        let lower = text.to_ascii_lowercase();
                        let link = (lower.starts_with("https://") || lower.starts_with("http://"))
                            && !text.chars().any(char::is_whitespace);
                        QrRowViewModel {
                            open: link.then(|| text.clone()),
                            copy: Some(text.clone()),
                            text,
                        }
                    }
                    QrPayload::Binary { bytes } => QrRowViewModel {
                        text: format!("Binary data ({bytes} bytes), not shown"),
                        open: None,
                        copy: None,
                    },
                })
                .collect::<Vec<_>>();
            let status = match rows.len() {
                0 => "no QR code was found.".to_owned(),
                1 => "1 code.".to_owned(),
                count => format!("{count} codes."),
            };
            (status, ScreenToolResultViewModel::Qr(rows))
        }
        ScreenToolResult::Color(color) => (
            color.hex(),
            ScreenToolResultViewModel::Color {
                rgb: [color.red, color.green, color.blue],
                hex: color.hex(),
                css_rgb: color.rgb(),
                hsl: color.hsl(),
            },
        ),
    }
}

#[cfg(test)]
mod tests {

    mod screen_tools {
        use crate::view_model::{
            QrRowViewModel, ScreenToolGate, ScreenToolResultViewModel, ScreenToolsPresentation,
            ScreenToolsViewModel,
        };
        use kestrel_platform::capture::{CaptureError, CaptureErrorKind};
        use kestrel_platform::screen_tools::{ALL_SCREEN_TOOLS, ScreenTool};
        use kestrel_services::screen_tools::{
            ColorSample, QrPayload, ScreenToolResult, ScreenToolsPhase, ScreenToolsSnapshot,
        };

        fn view(phase: ScreenToolsPhase, running: &[ScreenTool]) -> ScreenToolsViewModel {
            ScreenToolsViewModel::from_presentation(ScreenToolsPresentation {
                snapshot: ScreenToolsSnapshot {
                    phase,
                    generation: 1,
                },
                gates: ALL_SCREEN_TOOLS
                    .into_iter()
                    .map(|tool| {
                        let running = running.contains(&tool);
                        ScreenToolGate {
                            tool,
                            running,
                            reason: (!running).then(|| format!("{} is off", tool.feature_id())),
                        }
                    })
                    .collect(),
                settings: kestrel_core::ScreenToolsConfiguration::default(),
            })
        }

        #[test]
        fn each_tool_is_gated_on_its_own_feature() {
            let view = view(ScreenToolsPhase::Idle, &[ScreenTool::Qr]);
            let enabled = view
                .buttons
                .iter()
                .map(|button| (button.tool, button.enabled, button.reason.is_some()))
                .collect::<Vec<_>>();
            assert_eq!(
                enabled,
                [
                    (ScreenTool::Text, false, true),
                    (ScreenTool::Qr, true, false),
                    (ScreenTool::Color, false, true),
                ]
            );
            assert!(view.available && !view.busy);
            assert!(!self::view(ScreenToolsPhase::Idle, &[]).available);
        }

        #[test]
        fn a_waiting_frame_asks_for_the_selection_view_and_failures_explain() {
            let selecting = view(
                ScreenToolsPhase::Selecting(ScreenTool::Text),
                &ALL_SCREEN_TOOLS,
            );
            assert_eq!(selecting.selecting, Some(ScreenTool::Text));
            assert!(selecting.busy);
            let failed = view(
                ScreenToolsPhase::Failed(
                    ScreenTool::Text,
                    CaptureError::new(CaptureErrorKind::Denied, "The desktop denied it."),
                ),
                &ALL_SCREEN_TOOLS,
            );
            assert!(failed.status.contains("The desktop denied it."));
            assert_eq!((failed.selecting, failed.result), (None, None));
        }

        #[test]
        fn only_plain_web_addresses_open_and_binary_codes_stay_hidden() {
            let view = view(
                ScreenToolsPhase::Done(
                    ScreenTool::Qr,
                    ScreenToolResult::Qr(vec![
                        QrPayload::Text("HTTPS://example.org/a".into()),
                        QrPayload::Text("https://example.org/a b".into()),
                        QrPayload::Text("javascript:alert(1)".into()),
                        QrPayload::Binary { bytes: 9 },
                    ]),
                ),
                &ALL_SCREEN_TOOLS,
            );
            let Some(ScreenToolResultViewModel::Qr(rows)) = view.result else {
                panic!("QR rows expected");
            };
            let opens = rows
                .iter()
                .map(|row| row.open.is_some())
                .collect::<Vec<_>>();
            assert_eq!(opens, [true, false, false, false]);
            let QrRowViewModel { text, copy, .. } = &rows[3];
            assert_eq!(copy, &None);
            assert!(text.contains("9 bytes"), "{text}");
        }

        #[test]
        fn picked_colors_show_every_format() {
            let view = view(
                ScreenToolsPhase::Done(
                    ScreenTool::Color,
                    ScreenToolResult::Color(ColorSample {
                        red: 51,
                        green: 102,
                        blue: 204,
                    }),
                ),
                &ALL_SCREEN_TOOLS,
            );
            assert_eq!(
                view.result,
                Some(ScreenToolResultViewModel::Color {
                    rgb: [51, 102, 204],
                    hex: "#3366cc".into(),
                    css_rgb: "rgb(51, 102, 204)".into(),
                    hsl: "hsl(220, 60%, 50%)".into(),
                })
            );
        }
    }
}
