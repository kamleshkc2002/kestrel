//! Process and listener runtime features.

use super::*;

/// Text for a process action confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessConfirmation {
    /// Executable, PID, owner, and signal or restart scope.
    pub scope: String,
    /// A desktop restart may send SIGKILL to members still running.
    pub can_escalate: bool,
}

fn describe_sample_error(error: kestrel_platform::processes::ProcessError) -> String {
    use kestrel_platform::processes::ProcessErrorKind;
    match error.kind {
        ProcessErrorKind::Reused => ActionOutcome::Reused,
        ProcessErrorKind::Exited => ActionOutcome::AlreadyExited,
        _ => ActionOutcome::Failed(error),
    }
    .to_string()
}

/// Real UID of this process; actions apply only to its processes.
fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// Runs a confirmed process action. Callers hold no runtime lock: a
/// termination waits up to its grace period.
pub fn run_process_action(
    identity: kestrel_platform::processes::ProcessIdentity,
    action: crate::ProcessActionKind,
) -> String {
    let source = ProcfsProcesses::new("/proc");
    let signaller = PidfdSignaller::for_current_user(ProcfsProcesses::new("/proc"));
    let own_pid = std::process::id();
    let uid = current_uid();
    let eligibility = |sample: &kestrel_platform::processes::ProcessSample| {
        ProcfsProcesses::eligibility(sample, uid, own_pid)
    };
    let outcome = match action {
        crate::ProcessActionKind::Terminate => terminate(
            &source,
            &signaller,
            eligibility,
            identity,
            PROCESS_TERMINATE_GRACE,
        ),
        crate::ProcessActionKind::Kill => kill(&source, &signaller, eligibility, identity),
        crate::ProcessActionKind::Restart { escalate } => restart(
            &source,
            &signaller,
            &SystemRestarter::new(),
            eligibility,
            ProcfsProcesses::restart_target,
            identity,
            PROCESS_STOP_TIMEOUT,
            escalate,
        ),
    };
    outcome.to_string()
}

const PROCESS_TERMINATE_GRACE: Duration = Duration::from_secs(3);
const PROCESS_STOP_TIMEOUT: Duration = Duration::from_secs(5);

impl ApplicationRuntime {
    pub fn processes_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == PROCESSES_ID && registration.running)
    }

    fn require_processes(&self) -> Result<(), String> {
        self.processes_is_running().then_some(()).ok_or_else(|| {
            "Processes are not running; enable system.processes in the Feature Hub.".to_owned()
        })
    }

    pub fn listeners_is_running(&self) -> bool {
        self.registry.registrations().any(|registration| {
            registration.feature.id == LISTENERS_FEATURE_ID && registration.running
        })
    }

    fn require_listeners(&self) -> Result<(), String> {
        self.listeners_is_running().then_some(()).ok_or_else(|| {
            "Listening ports are not running; enable network.listeners in the Feature Hub."
                .to_owned()
        })
    }

    pub fn process_snapshot(&self) -> ProcessSnapshot {
        self.process_inventory.snapshot()
    }

    pub fn processes_view_model(&self, query: &str) -> ProcessesViewModel {
        ProcessesViewModel::from_presentation(crate::view_model::ProcessesPresentation {
            snapshot: self.process_snapshot(),
            running: self.processes_is_running(),
            query: query.to_owned(),
            current_uid: current_uid(),
            own_pid: std::process::id(),
            limits: self.process_limits,
            signalling: self.process_signalling,
        })
    }

    pub fn refresh_processes(&self) -> Result<(), String> {
        self.require_processes()?;
        self.process_inventory.refresh_now();
        Ok(())
    }

    pub fn listeners_view_model(&self) -> ListenersViewModel {
        ListenersViewModel::from_presentation(crate::view_model::ListenersPresentation {
            snapshot: self.listeners.snapshot(),
            running: self.listeners_is_running(),
            signalling: self.process_signalling,
        })
    }

    pub fn listener_generation(&self) -> u64 {
        self.listeners.generation()
    }

    pub fn refresh_listeners(&mut self) -> Result<(), String> {
        self.require_listeners()?;
        self.listeners.refresh(
            &ProcfsListeners::new("/proc"),
            kestrel_platform::processes::MAX_LISTENERS,
        );
        Ok(())
    }

    pub fn process_confirmation(
        &self,
        identity: kestrel_platform::processes::ProcessIdentity,
        action: crate::ProcessActionKind,
    ) -> Result<ProcessConfirmation, String> {
        self.require_processes()?;
        if !self.process_signalling {
            return Err(kestrel_platform::processes::NO_PIDFD_EXPLANATION.to_owned());
        }
        let source = ProcfsProcesses::new("/proc");
        let sample = source.sample(identity).map_err(describe_sample_error)?;
        ProcfsProcesses::eligibility(&sample, current_uid(), std::process::id())
            .map_err(|reason| reason.explanation().to_owned())?;
        let (scope, can_escalate) = match action {
            crate::ProcessActionKind::Terminate => {
                (confirmation_scope(&sample, ConfirmAction::Terminate), false)
            }
            crate::ProcessActionKind::Kill => {
                (confirmation_scope(&sample, ConfirmAction::Kill), false)
            }
            crate::ProcessActionKind::Restart { .. } => {
                let target = ProcfsProcesses::restart_target(&sample)
                    .ok_or_else(|| "This process has no supported restart target.".to_owned())?;
                let members = source
                    .scope_members(identity)
                    .map_err(describe_sample_error)?
                    .len();
                let can_escalate = matches!(target, RestartTarget::DesktopApp { .. });
                (
                    confirmation_scope(&sample, ConfirmAction::Restart(&target, members)),
                    can_escalate,
                )
            }
        };
        Ok(ProcessConfirmation {
            scope,
            can_escalate,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabling_process_features_joins_the_worker_and_refuses_actions() {
        use super::{LISTENERS_FEATURE_ID, PROCESSES_ID};
        let mut configuration = ApplicationConfiguration::default();
        for feature in [PROCESSES_ID, LISTENERS_FEATURE_ID] {
            configuration
                .set_feature_enabled(feature, true)
                .expect("valid feature ID");
        }
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");
        runtime.start();
        assert!(runtime.process_inventory.is_running());
        let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback binds");
        runtime.refresh_listeners().expect("listeners are enabled");
        assert!(!runtime.listeners.snapshot().listeners.is_empty());

        runtime
            .apply_configuration(&ApplicationConfiguration::default())
            .expect("configuration applies");
        drop(socket);

        assert!(!runtime.process_inventory.is_running(), "worker joined");
        assert!(runtime.listeners.snapshot().listeners.is_empty());
        assert!(runtime.refresh_processes().is_err());
        assert!(runtime.refresh_listeners().is_err());
        let own = kestrel_platform::processes::ProcessIdentity {
            pid: std::process::id(),
            start_ticks: 0,
        };
        let refused = runtime
            .process_confirmation(own, crate::ProcessActionKind::Kill)
            .expect_err("a disabled feature confirms nothing");
        assert!(refused.contains(PROCESSES_ID), "{refused}");
        assert!(runtime.processes_view_model("").rows.is_empty());
    }
}
