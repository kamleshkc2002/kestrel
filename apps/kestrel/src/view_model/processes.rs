//! Process and listener presentation.

use super::*;

/// Inputs for the process panel; `current_uid` and `own_pid` decide which
/// rows offer actions.
pub(crate) struct ProcessesPresentation {
    pub snapshot: ProcessSnapshot,
    pub running: bool,
    pub query: String,
    pub current_uid: u32,
    pub own_pid: u32,
    pub limits: kestrel_core::ProcessConfiguration,
    /// `pidfd_open` works; without it no row offers an action.
    pub signalling: bool,
}

pub(crate) struct ListenersPresentation {
    pub snapshot: ListenerSnapshot,
    pub running: bool,
    /// `pidfd_open` works; without it Stop owner is hidden.
    pub signalling: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessActionViewModel {
    pub action: ProcessActionKind,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRowViewModel {
    pub identity: ProcessIdentity,
    pub title: String,
    pub subtitle: String,
    pub actions: Vec<ProcessActionViewModel>,
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessesViewModel {
    pub running: bool,
    pub status: String,
    pub rows: Vec<ProcessRowViewModel>,
    pub query: String,
    /// Configured cadence and row bound, shown in Settings.
    pub limits: kestrel_core::ProcessConfiguration,
    pub stale_notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerRowViewModel {
    pub title: String,
    pub owner: String,
    pub owner_identity: Option<ProcessIdentity>,
    pub port: u16,
    pub address: String,
    pub pid: Option<u32>,
    pub open_url: Option<String>,
    pub stop_disabled_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenersViewModel {
    pub running: bool,
    pub status: String,
    pub rows: Vec<ListenerRowViewModel>,
    /// Stop owner is offered only when process signalling works.
    pub can_stop: bool,
}

impl ProcessesViewModel {
    pub(crate) fn from_presentation(presentation: ProcessesPresentation) -> Self {
        let rows = if presentation.running {
            let limit = presentation.limits.max_rows as usize;
            search_processes(&presentation.snapshot.rows, &presentation.query, limit)
                .into_iter()
                .map(|row| {
                    let mut row = process_row(row, presentation.current_uid, presentation.own_pid);
                    if !presentation.signalling {
                        row.actions.clear();
                    }
                    row
                })
                .collect()
        } else {
            Vec::new()
        };
        let status = if !presentation.running {
            "Processes are off. Enable system.processes in the Feature Hub.".to_owned()
        } else if let Some(error) = &presentation.snapshot.error {
            format!("Process scan failed: {}", error.message)
        } else {
            format!(
                "{} process(es){}.{}",
                presentation.snapshot.total_processes,
                if presentation.snapshot.truncated {
                    "; list truncated"
                } else {
                    ""
                },
                signalling_note(presentation.signalling)
            )
        };
        let stale_notice = presentation.snapshot.stale.then(|| {
            presentation
                .snapshot
                .error
                .as_ref()
                .map(|error| {
                    format!(
                        "Showing the last process list; refresh failed: {}",
                        error.message
                    )
                })
                .unwrap_or_else(|| "Showing a stale process list; refresh failed.".to_owned())
        });
        Self {
            running: presentation.running,
            status,
            rows,
            query: presentation.query,
            limits: presentation.limits,
            stale_notice,
        }
    }
}

fn signalling_note(signalling: bool) -> String {
    if signalling {
        String::new()
    } else {
        format!(" {NO_PIDFD_EXPLANATION}")
    }
}

fn process_row(row: &ProcessRow, current_uid: u32, own_pid: u32) -> ProcessRowViewModel {
    let sample = &row.sample;
    let title = sample
        .executable
        .as_deref()
        .filter(|executable| *executable != sample.name)
        .map_or_else(|| sample.name.clone(), str::to_owned);
    let cpu = row.cpu_percent.map_or_else(
        || "CPU unavailable".to_owned(),
        |value| format!("CPU {value:.1}%"),
    );
    let owner = sample.owner.as_deref().unwrap_or("unknown user");
    let subtitle = format!(
        "PID {} · start {} · {} · {} · {}",
        sample.identity.pid,
        sample.identity.start_ticks,
        owner,
        cpu,
        format_bytes(sample.resident_bytes as f64)
    );
    let eligibility = ProcfsProcesses::eligibility(sample, current_uid, own_pid);
    let mut actions = Vec::new();
    let disabled_reason = eligibility
        .err()
        .map(Ineligible::explanation)
        .map(str::to_owned);
    if disabled_reason.is_none() {
        actions.push(ProcessActionViewModel {
            action: ProcessActionKind::Terminate,
            label: "End with SIGTERM".to_owned(),
        });
        actions.push(ProcessActionViewModel {
            action: ProcessActionKind::Kill,
            label: "Kill with SIGKILL".to_owned(),
        });
        if let Some(target) = ProcfsProcesses::restart_target(sample) {
            let label = match target {
                RestartTarget::UserUnit { unit } => format!("Restart {unit}"),
                RestartTarget::DesktopApp { desktop_id, .. } => format!("Restart {desktop_id}"),
            };
            actions.push(ProcessActionViewModel {
                action: ProcessActionKind::Restart { escalate: false },
                label,
            });
        }
    }
    ProcessRowViewModel {
        identity: sample.identity,
        title,
        subtitle,
        actions,
        disabled_reason,
    }
}

impl ListenersViewModel {
    pub(crate) fn from_presentation(presentation: ListenersPresentation) -> Self {
        let rows: Vec<ListenerRowViewModel> = if presentation.running {
            presentation
                .snapshot
                .listeners
                .iter()
                .map(listener_row)
                .collect()
        } else {
            Vec::new()
        };
        let status = if !presentation.running {
            "Listening ports are off. Enable network.listeners in the Feature Hub.".to_owned()
        } else if let Some(error) = &presentation.snapshot.error {
            format!("Listener scan failed: {}", error.message)
        } else {
            format!(
                "{} listening port(s).{}",
                rows.len(),
                signalling_note(presentation.signalling)
            )
        };
        Self {
            running: presentation.running,
            status,
            rows,
            can_stop: presentation.signalling,
        }
    }
}

fn listener_row(listener: &Listener) -> ListenerRowViewModel {
    let address = listener.address.to_string();
    let protocol = match listener.protocol {
        ListenerProtocol::Tcp => "TCP",
        ListenerProtocol::Udp => "UDP",
    };
    let socket = std::net::SocketAddr::new(listener.address, listener.port);
    let title = format!("{protocol} {socket}");
    let owner = listener
        .owner_name
        .clone()
        .unwrap_or_else(|| "Unknown owner".to_owned());
    // Loopback listeners open at their own address; wildcard ones through localhost.
    let open_url = match listener.protocol {
        ListenerProtocol::Tcp if listener.address.is_unspecified() => {
            Some(format!("http://localhost:{}/", listener.port))
        }
        ListenerProtocol::Tcp if listener.address.is_loopback() => {
            Some(format!("http://{socket}/"))
        }
        _ => None,
    };
    ListenerRowViewModel {
        title,
        owner,
        owner_identity: listener.owner,
        port: listener.port,
        address,
        pid: listener.owner.map(|identity| identity.pid),
        open_url,
        stop_disabled_reason: listener.owner.is_none().then(|| {
            "The owner belongs to another user or has exited; refresh, or stop it from \
             that user's session."
                .to_owned()
        }),
    }
}

impl Default for ProcessesViewModel {
    fn default() -> Self {
        Self {
            running: false,
            status: "Processes are off.".to_owned(),
            rows: Vec::new(),
            query: String::new(),
            limits: kestrel_core::ProcessConfiguration::default(),
            stale_notice: None,
        }
    }
}

impl Default for ListenersViewModel {
    fn default() -> Self {
        Self {
            running: false,
            status: "Listening ports are off.".to_owned(),
            rows: Vec::new(),
            can_stop: false,
        }
    }
}

#[cfg(test)]
mod tests {

    mod processes {
        use crate::view_model::{
            ListenersPresentation, ListenersViewModel, ProcessActionKind, ProcessesPresentation,
            ProcessesViewModel,
        };
        use kestrel_platform::processes::{
            Listener, ListenerProtocol, ProcessIdentity, ProcessKind, ProcessSample, ProcessScope,
        };
        use kestrel_services::processes::{ListenerSnapshot, ProcessRow, ProcessSnapshot};

        const USER: u32 = 1000;
        const KESTREL: u32 = 4242;

        fn sample(pid: u32, name: &str, uid: Option<u32>, scope: ProcessScope) -> ProcessRow {
            ProcessRow {
                sample: ProcessSample {
                    identity: ProcessIdentity {
                        pid,
                        start_ticks: u64::from(pid) * 10,
                    },
                    parent_pid: 1,
                    name: name.to_owned(),
                    executable: Some(name.to_owned()),
                    uid,
                    owner: uid.map(|uid| if uid == USER { "me" } else { "root" }.to_owned()),
                    kind: ProcessKind::User,
                    state: 'S',
                    cpu_ticks: 0,
                    resident_bytes: 4096,
                    threads: 1,
                    scope_root: scope != ProcessScope::Other,
                    scope,
                },
                cpu_percent: Some(1.0),
            }
        }

        fn view(rows: Vec<ProcessRow>, running: bool, query: &str) -> ProcessesViewModel {
            view_with_signalling(rows, running, query, true)
        }

        fn view_with_signalling(
            rows: Vec<ProcessRow>,
            running: bool,
            query: &str,
            signalling: bool,
        ) -> ProcessesViewModel {
            ProcessesViewModel::from_presentation(ProcessesPresentation {
                snapshot: ProcessSnapshot {
                    total_processes: rows.len(),
                    rows,
                    ..ProcessSnapshot::default()
                },
                running,
                query: query.to_owned(),
                current_uid: USER,
                own_pid: KESTREL,
                limits: kestrel_core::ProcessConfiguration::default(),
                signalling,
            })
        }

        fn actions(view: &ProcessesViewModel, pid: u32) -> Vec<ProcessActionKind> {
            view.rows
                .iter()
                .find(|row| row.identity.pid == pid)
                .expect("row is listed")
                .actions
                .iter()
                .map(|action| action.action)
                .collect()
        }

        #[test]
        fn only_own_eligible_processes_offer_actions() {
            let mut kernel = sample(2, "kthreadd", Some(0), ProcessScope::Other);
            kernel.sample.kind = ProcessKind::KernelThread;
            let view = view(
                vec![
                    sample(100, "editor", Some(USER), ProcessScope::Other),
                    sample(200, "sshd", Some(0), ProcessScope::Other),
                    sample(300, "ghost", None, ProcessScope::Other),
                    sample(KESTREL, "kestrel", Some(USER), ProcessScope::Other),
                    sample(1, "systemd", Some(USER), ProcessScope::Other),
                    kernel,
                ],
                true,
                "",
            );

            assert_eq!(
                actions(&view, 100),
                [ProcessActionKind::Terminate, ProcessActionKind::Kill]
            );
            for pid in [200, 300, KESTREL, 1, 2] {
                let row = view
                    .rows
                    .iter()
                    .find(|row| row.identity.pid == pid)
                    .unwrap();
                assert!(row.actions.is_empty(), "PID {pid} must not be actionable");
                assert!(row.disabled_reason.is_some(), "PID {pid} explains why");
            }
            let editor = view
                .rows
                .iter()
                .find(|row| row.identity.pid == 100)
                .unwrap();
            assert!(
                editor.subtitle.contains("start 1000"),
                "{}",
                editor.subtitle
            );
        }

        #[test]
        fn restart_requires_a_unit_or_desktop_launch_scope() {
            let view = view(
                vec![
                    sample(
                        10,
                        "pipewire",
                        Some(USER),
                        ProcessScope::UserUnit {
                            unit: "pipewire.service".to_owned(),
                        },
                    ),
                    sample(
                        11,
                        "firefox",
                        Some(USER),
                        ProcessScope::DesktopLaunch {
                            desktop_id: "firefox.desktop".to_owned(),
                            scope: "app-gnome-firefox-11.scope".to_owned(),
                        },
                    ),
                    sample(12, "bash", Some(USER), ProcessScope::Other),
                ],
                true,
                "",
            );
            let restart = ProcessActionKind::Restart { escalate: false };
            assert!(actions(&view, 10).contains(&restart));
            assert!(actions(&view, 11).contains(&restart));
            assert!(!actions(&view, 12).contains(&restart));
        }

        #[test]
        fn without_pidfd_no_process_or_listener_offers_an_action() {
            let unit = ProcessScope::UserUnit {
                unit: "pipewire.service".to_owned(),
            };
            let processes = view_with_signalling(
                vec![sample(10, "pipewire", Some(USER), unit)],
                true,
                "",
                false,
            );
            assert!(processes.rows[0].actions.is_empty());
            assert!(processes.status.contains("pidfd"), "{}", processes.status);

            let listeners = ListenersViewModel::from_presentation(ListenersPresentation {
                snapshot: ListenerSnapshot::default(),
                running: true,
                signalling: false,
            });
            assert!(!listeners.can_stop);
            assert!(listeners.status.contains("pidfd"), "{}", listeners.status);
        }

        #[test]
        fn search_filters_rows_and_a_stopped_feature_lists_none() {
            let rows = vec![
                sample(100, "editor", Some(USER), ProcessScope::Other),
                sample(200, "compiler", Some(USER), ProcessScope::Other),
            ];
            let filtered = view(rows.clone(), true, "comp");
            assert_eq!(
                filtered
                    .rows
                    .iter()
                    .map(|row| row.identity.pid)
                    .collect::<Vec<_>>(),
                [200]
            );
            let stopped = view(rows, false, "");
            assert!(stopped.rows.is_empty());
            assert!(
                stopped.status.contains("system.processes"),
                "{}",
                stopped.status
            );
        }

        #[test]
        fn listeners_without_a_verified_owner_cannot_be_stopped() {
            let owned = Listener {
                protocol: ListenerProtocol::Tcp,
                address: "127.0.0.1".parse().unwrap(),
                port: 8765,
                owner: Some(ProcessIdentity {
                    pid: 100,
                    start_ticks: 1,
                }),
                owner_name: Some("python3".to_owned()),
            };
            let foreign = Listener {
                protocol: ListenerProtocol::Udp,
                address: "0.0.0.0".parse().unwrap(),
                port: 5353,
                owner: None,
                owner_name: None,
            };
            let view = ListenersViewModel::from_presentation(ListenersPresentation {
                snapshot: ListenerSnapshot {
                    listeners: vec![owned, foreign],
                    ..ListenerSnapshot::default()
                },
                running: true,
                signalling: true,
            });

            let [owned, foreign] = view.rows.as_slice() else {
                panic!("two rows expected");
            };
            assert_eq!(owned.pid, Some(100));
            assert!(owned.stop_disabled_reason.is_none());
            assert_eq!(owned.open_url.as_deref(), Some("http://127.0.0.1:8765/"));
            assert_eq!(foreign.owner_identity, None);
            assert!(foreign.stop_disabled_reason.is_some());
            assert!(view.can_stop);
            assert_eq!(foreign.open_url, None, "UDP has no browser action");
        }
    }
}
