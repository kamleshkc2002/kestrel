mod window;

use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use adw::{glib, prelude::*};
use async_channel::{Receiver, Sender};
use kestrel::{
    AlertKind, AppearancePreference, ApplicationCommand, ApplicationRuntime, ApplicationViewModel,
    ConfigurationWarning, FeaturePreset, LoadedConfiguration, MonitorReadout, MonitorViewModel,
    PanelMoveDirection, PanelSection, StatusNotifierIntegration, configuration_path,
    disable as disable_autostart, enable as enable_autostart, export_file, import_file, load, save,
};
use kestrel_core::{
    ApplicationConfiguration, FeatureConfigurationSnapshot, PanelSectionConfiguration,
};
use kestrel_platform::notifications::{AlertNotification, AlertNotifier, DesktopNotifier};
use kestrel_services::alerts::notification_text;

use window::WindowView;

const MONITOR_TICK_INTERVAL: Duration = Duration::from_millis(250);

type RefreshResult = Result<(ApplicationViewModel, String), String>;
/// `None` means the tick produced no new snapshot, so the window is left untouched.
type MonitorResult = Option<MonitorViewModel>;

type SharedState = Arc<Mutex<ControllerState>>;

struct ControllerState {
    runtime: ApplicationRuntime,
    configuration: ApplicationConfiguration,
    warnings: Vec<ConfigurationWarning>,
    preset_snapshot: Option<FeatureConfigurationSnapshot>,
}

impl ControllerState {
    fn view_model(&self) -> ApplicationViewModel {
        self.runtime.view_model(
            &self.warnings,
            &self.configuration,
            self.preset_snapshot.is_some(),
        )
    }
}

/// Samples the monitor, delivers alert notifications, and reports owned monitoring
/// presentation. The state mutex is held only for the sample and for recording
/// delivery results, never across the blocking session-bus notification call.
fn run_monitor_tick(
    state: &SharedState,
    notifier: &dyn AlertNotifier,
    observed_at: Duration,
) -> MonitorResult {
    let (updated, alerts) = {
        let mut guard = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tick = guard.runtime.sample_monitor(observed_at);
        (
            tick.outcome == kestrel_services::system_monitor::RefreshOutcome::Updated,
            tick.alerts,
        )
    };
    if !updated {
        return None;
    }

    let deliveries = alerts
        .iter()
        .map(|event| {
            let (summary, body) = notification_text(event);
            let result = notifier
                .notify(&AlertNotification { summary, body })
                .map_err(|error| error.to_string());
            (event.kind(), result)
        })
        .collect::<Vec<_>>();

    let mut guard = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (kind, result) in deliveries {
        guard.runtime.record_alert_delivery(kind, result);
    }
    Some(guard.runtime.monitor_view_model(&guard.configuration))
}

enum ControllerOperation {
    Refresh,
    QuickToggle(kestrel::QuickToggleCommand),
    SetFeatureEnabled {
        feature_id: String,
        enabled: bool,
    },
    ApplyPreset(FeaturePreset),
    UndoPreset,
    SetAppearance(AppearancePreference),
    SetAutostart(bool),
    SetPanelVisibility {
        section: PanelSection,
        visible: bool,
    },
    MovePanelSection {
        section: PanelSection,
        direction: PanelMoveDirection,
    },
    SetMonitorReadoutVisible {
        readout: MonitorReadout,
        visible: bool,
    },
    MoveMonitorReadout {
        readout: MonitorReadout,
        direction: PanelMoveDirection,
    },
    SetAlertEnabled {
        kind: AlertKind,
        enabled: bool,
    },
    SetAlertThreshold {
        kind: AlertKind,
        threshold: f64,
    },
    ImportConfiguration(PathBuf),
    ExportConfiguration(PathBuf),
}

/// Channels, paths, and adapters that the application loop hands to the controller.
struct ControllerContext {
    config_path: Option<PathBuf>,
    refresh_results: Sender<RefreshResult>,
    monitor_results: Sender<MonitorResult>,
    commands: Sender<ApplicationCommand>,
    notifier: Arc<dyn AlertNotifier>,
}

struct ApplicationController {
    state: SharedState,
    config_path: Option<PathBuf>,
    window: RefCell<Option<WindowView>>,
    refreshing: Cell<bool>,
    monitoring: Cell<bool>,
    /// Mirrors the monitor registration so a disabled feature never spawns tick workers.
    monitor_running: Cell<bool>,
    monitor_started_at: Instant,
    notifier: Arc<dyn AlertNotifier>,
    refresh_results: Sender<RefreshResult>,
    monitor_results: Sender<MonitorResult>,
    commands: Sender<ApplicationCommand>,
}
impl ApplicationController {
    fn new(
        runtime: ApplicationRuntime,
        configuration: ApplicationConfiguration,
        warnings: Vec<ConfigurationWarning>,
        context: ControllerContext,
    ) -> Rc<Self> {
        let ControllerContext {
            config_path,
            refresh_results,
            monitor_results,
            commands,
            notifier,
        } = context;
        let monitor_running = runtime.monitor_is_running();
        Rc::new(Self {
            state: Arc::new(Mutex::new(ControllerState {
                runtime,
                configuration,
                warnings,
                preset_snapshot: None,
            })),
            config_path,
            window: RefCell::new(None),
            refreshing: Cell::new(false),
            monitoring: Cell::new(false),
            monitor_running: Cell::new(monitor_running),
            monitor_started_at: Instant::now(),
            notifier,
            refresh_results,
            monitor_results,
            commands,
        })
    }

    fn present(self: &Rc<Self>, application: &adw::Application) {
        let existing = self
            .window
            .borrow()
            .as_ref()
            .map(|view| view.window.clone());
        if let Some(window) = existing {
            window.present();
            return;
        }

        let view_model = self.current_view_model();
        let view = WindowView::new(application, &view_model, self.commands.clone());
        let weak_controller = Rc::downgrade(self);
        view.window.connect_close_request(move |_| {
            if let Some(controller) = weak_controller.upgrade() {
                controller.window.borrow_mut().take();
            }
            glib::Propagation::Proceed
        });
        view.window.present();
        self.window.replace(Some(view));
    }

    fn request_refresh(&self) {
        self.request_operation(ControllerOperation::Refresh, "kestrel-capability-refresh");
    }

    fn request_quick_toggle(&self, command: kestrel::QuickToggleCommand) {
        self.request_operation(
            ControllerOperation::QuickToggle(command),
            "kestrel-quick-toggle",
        );
    }

    fn request_operation(&self, operation: ControllerOperation, worker_name: &'static str) {
        if self.refreshing.replace(true) {
            if let Some(view) = self.window.borrow().as_ref() {
                view.show_message("Another Kestrel operation is still running");
            }
            return;
        }
        if let Some(view) = self.window.borrow().as_ref() {
            view.set_refreshing(true);
        }

        let state = Arc::clone(&self.state);
        let config_path = self.config_path.clone();
        let results = self.refresh_results.clone();
        let worker = thread::Builder::new()
            .name(worker_name.to_owned())
            .spawn(move || {
                let result = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .run(operation, config_path.as_deref());
                let _ = results.send_blocking(result);
            });
        if let Err(error) = worker {
            self.finish_refresh(Err(format!(
                "Could not start the {worker_name} worker: {error}"
            )));
        }
    }

    fn finish_refresh(&self, result: RefreshResult) {
        self.refreshing.set(false);
        let window = self.window.borrow();
        let Some(view) = window.as_ref() else {
            return;
        };
        view.set_refreshing(false);
        match result {
            Ok((view_model, message)) => {
                apply_color_scheme(view_model.appearance);
                self.monitor_running.set(view_model.monitor.running);
                view.set_view_model(&view_model);
                view.show_message(&message);
            }
            Err(message) => {
                let view_model = self.current_view_model();
                self.monitor_running.set(view_model.monitor.running);
                view.set_view_model(&view_model);
                view.show_message(&message);
            }
        }
    }
    fn request_monitor_sample(&self) {
        if self.refreshing.get() || !self.monitor_running.get() || self.monitoring.replace(true) {
            return;
        }
        let state = Arc::clone(&self.state);
        let notifier = Arc::clone(&self.notifier);
        let results = self.monitor_results.clone();
        let observed_at = self.monitor_started_at.elapsed();
        let worker = thread::Builder::new()
            .name("kestrel-monitor-sample".to_owned())
            .spawn(move || {
                let view_model = run_monitor_tick(&state, notifier.as_ref(), observed_at);
                let _ = results.send_blocking(view_model);
            });
        if worker.is_err() {
            self.monitoring.set(false);
        }
    }

    fn finish_monitor_sample(&self, monitor: Option<MonitorViewModel>) {
        self.monitoring.set(false);
        let Some(monitor) = monitor else {
            return;
        };
        self.monitor_running.set(monitor.running);
        if let Some(view) = self.window.borrow().as_ref() {
            view.set_monitor(&monitor);
        }
    }

    fn current_view_model(&self) -> ApplicationViewModel {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .view_model()
    }
}

impl ControllerState {
    fn run(&mut self, operation: ControllerOperation, config_path: Option<&Path>) -> RefreshResult {
        let message = match operation {
            ControllerOperation::Refresh => {
                self.runtime
                    .refresh_capabilities()
                    .map_err(|error| format!("Capability refresh failed: {error:?}"))?;
                "Capability and toggle state refreshed".to_owned()
            }
            ControllerOperation::QuickToggle(command) => {
                let label = command.id.label();
                match self.runtime.execute_quick_toggle_command(command) {
                    Some(Ok(_)) => format!("{label} updated"),
                    Some(Err(error)) => format!("{label} failed: {error}"),
                    None => format!("{label} is not available"),
                }
            }
            ControllerOperation::SetFeatureEnabled {
                feature_id,
                enabled,
            } => {
                let old = self.configuration.clone();
                let mut candidate = old.clone();
                if let Err(error) = candidate.set_feature_enabled(feature_id.clone(), enabled) {
                    return Err(format!("Could not update feature {feature_id}: {error:?}"));
                }
                self.runtime
                    .apply_configuration(&candidate)
                    .map_err(|error| {
                        self.restore_configuration(old.clone());
                        format!("Could not apply feature {feature_id}: {error:?}")
                    })?;
                if let Err(error) = persist(config_path, &candidate) {
                    self.restore_configuration(old);
                    return Err(format!("Feature change was not saved: {error}"));
                }
                self.configuration = candidate;
                self.preset_snapshot = None;
                format!(
                    "{feature_id} {}",
                    if enabled { "enabled" } else { "disabled" }
                )
            }
            ControllerOperation::ApplyPreset(preset) => {
                let old = self.configuration.clone();
                let snapshot = self
                    .runtime
                    .apply_preset(&mut self.configuration, preset)
                    .map_err(|error| {
                        self.restore_configuration(old.clone());
                        format!("Could not apply the {} preset: {error:?}", preset.label())
                    })?;
                if let Err(error) = persist(config_path, &self.configuration) {
                    self.restore_configuration(old);
                    return Err(format!("Preset was not saved: {error}"));
                }
                self.preset_snapshot = Some(snapshot);
                format!("{} preset applied", preset.label())
            }
            ControllerOperation::UndoPreset => {
                let Some(snapshot) = self.preset_snapshot.take() else {
                    return Err("There is no feature preset to undo".to_owned());
                };
                let old = self.configuration.clone();
                let snapshot_for_error = snapshot.clone();
                if let Err(error) = self.runtime.undo_preset(&mut self.configuration, snapshot) {
                    self.preset_snapshot = Some(snapshot_for_error);
                    self.restore_configuration(old);
                    return Err(format!("Could not undo the feature preset: {error:?}"));
                }
                if let Err(error) = persist(config_path, &self.configuration) {
                    self.preset_snapshot = Some(snapshot_for_error);
                    self.restore_configuration(old);
                    return Err(format!("Preset undo was not saved: {error}"));
                }
                "Feature preset undone".to_owned()
            }
            ControllerOperation::SetAppearance(appearance) => {
                let old = self.configuration.clone();
                self.configuration.ui.appearance = appearance;
                if let Err(error) = persist(config_path, &self.configuration) {
                    self.configuration = old;
                    return Err(format!("Appearance change was not saved: {error}"));
                }

                "Appearance updated".to_owned()
            }
            ControllerOperation::SetAutostart(enabled) => {
                self.set_autostart(enabled, config_path)?;
                format!("Autostart {}", if enabled { "enabled" } else { "disabled" })
            }
            ControllerOperation::SetPanelVisibility { section, visible } => {
                let old = self.configuration.clone();
                let Some(panel) = self
                    .configuration
                    .ui
                    .panel_sections
                    .iter_mut()
                    .find(|panel| panel.section == section)
                else {
                    return Err(format!("Panel section {section:?} is not configured"));
                };
                panel.visible = visible;
                let message = format!(
                    "{} panel {}",
                    panel_label(section),
                    if visible { "shown" } else { "hidden" }
                );
                if let Err(error) = persist(config_path, &self.configuration) {
                    self.configuration = old;
                    return Err(format!("Panel visibility was not saved: {error}"));
                }

                message
            }
            ControllerOperation::MovePanelSection { section, direction } => {
                let old = self.configuration.clone();
                if !move_panel_section(
                    &mut self.configuration.ui.panel_sections,
                    section,
                    direction,
                ) {
                    return Ok((
                        self.view_model(),
                        format!("{} panel is already at that boundary", panel_label(section)),
                    ));
                }
                if let Err(error) = persist(config_path, &self.configuration) {
                    self.configuration = old;
                    return Err(format!("Panel order was not saved: {error}"));
                }

                "Panel order updated".to_owned()
            }
            ControllerOperation::SetMonitorReadoutVisible { readout, visible } => {
                let mut candidate = self.configuration.clone();
                if visible {
                    if !candidate.monitoring.readouts.contains(&readout) {
                        candidate.monitoring.readouts.push(readout);
                    }
                } else {
                    candidate
                        .monitoring
                        .readouts
                        .retain(|entry| *entry != readout);
                }
                self.commit_monitoring_configuration(
                    candidate,
                    config_path,
                    format!(
                        "{} readout {}",
                        readout.label(),
                        if visible { "shown" } else { "hidden" }
                    ),
                )?
            }
            ControllerOperation::MoveMonitorReadout { readout, direction } => {
                let mut candidate = self.configuration.clone();
                if !move_monitor_readout(&mut candidate.monitoring.readouts, readout, direction) {
                    return Ok((
                        self.view_model(),
                        format!("{} readout is already at that boundary", readout.label()),
                    ));
                }
                self.commit_monitoring_configuration(
                    candidate,
                    config_path,
                    "Monitor readout order updated".to_owned(),
                )?
            }
            ControllerOperation::SetAlertEnabled { kind, enabled } => {
                let mut candidate = self.configuration.clone();
                candidate.monitoring.alerts.rule_mut(kind).enabled = enabled;
                self.commit_monitoring_configuration(
                    candidate,
                    config_path,
                    format!(
                        "{} alerts {}",
                        kind.label(),
                        if enabled { "enabled" } else { "disabled" }
                    ),
                )?
            }
            ControllerOperation::SetAlertThreshold { kind, threshold } => {
                let mut candidate = self.configuration.clone();
                candidate.monitoring.alerts.rule_mut(kind).threshold = threshold;
                self.commit_monitoring_configuration(
                    candidate,
                    config_path,
                    format!("{} alert threshold updated", kind.label()),
                )?
            }
            ControllerOperation::ImportConfiguration(path) => {
                let imported = import_file(&path)
                    .map_err(|error| format!("Could not import {}: {error}", path.display()))?;
                let old_configuration = self.configuration.clone();
                let old_warnings = std::mem::replace(&mut self.warnings, imported.warnings);
                let candidate = imported.configuration;
                if let Err(error) = self.runtime.apply_configuration(&candidate) {
                    self.warnings = old_warnings;
                    self.restore_configuration(old_configuration.clone());
                    return Err(format!(
                        "Imported configuration could not be applied: {error:?}"
                    ));
                }
                let old_autostart = old_configuration.startup.autostart;
                let new_autostart = candidate.startup.autostart;
                if old_autostart != new_autostart {
                    if let Err(error) = set_autostart_side_effect(new_autostart) {
                        self.warnings = old_warnings;
                        self.restore_configuration(old_configuration.clone());
                        return Err(format!("Imported autostart preference failed: {error}"));
                    }
                }
                if let Err(error) = persist(config_path, &candidate) {
                    if old_autostart != new_autostart {
                        let _ = set_autostart_side_effect(old_autostart);
                    }
                    self.warnings = old_warnings;
                    self.restore_configuration(old_configuration);
                    return Err(format!("Imported configuration was not saved: {error}"));
                }
                self.configuration = candidate;
                self.preset_snapshot = None;
                format!("Imported configuration from {}", path.display())
            }
            ControllerOperation::ExportConfiguration(path) => {
                export_file(&path, &self.configuration)
                    .map_err(|error| format!("Could not export {}: {error}", path.display()))?;
                format!("Exported configuration to {}", path.display())
            }
        };
        Ok((self.view_model(), message))
    }
    fn commit_monitoring_configuration(
        &mut self,
        candidate: ApplicationConfiguration,
        config_path: Option<&Path>,
        message: String,
    ) -> Result<String, String> {
        candidate
            .validate()
            .map_err(|error| format!("Invalid monitoring configuration: {error:?}"))?;
        let old = self.configuration.clone();
        self.runtime
            .apply_configuration(&candidate)
            .map_err(|error| {
                self.restore_configuration(old.clone());
                format!("Could not apply monitoring configuration: {error:?}")
            })?;
        if let Err(error) = persist(config_path, &candidate) {
            self.restore_configuration(old);
            return Err(format!("Monitoring change was not saved: {error}"));
        }
        self.configuration = candidate;
        Ok(message)
    }

    fn restore_configuration(&mut self, configuration: ApplicationConfiguration) {
        self.configuration = configuration.clone();
        let _ = self.runtime.apply_configuration(&configuration);
    }

    fn set_autostart(&mut self, enabled: bool, config_path: Option<&Path>) -> Result<(), String> {
        let old = self.configuration.clone();
        if old.startup.autostart != enabled {
            set_autostart_side_effect(enabled)
                .map_err(|error| format!("Could not change autostart: {error}"))?;
        }
        self.configuration.startup.autostart = enabled;
        if let Err(error) = persist(config_path, &self.configuration) {
            if old.startup.autostart != enabled {
                let _ = set_autostart_side_effect(old.startup.autostart);
            }
            self.configuration = old;
            return Err(format!("Autostart change was not saved: {error}"));
        }

        Ok(())
    }
}

fn persist(path: Option<&Path>, configuration: &ApplicationConfiguration) -> Result<(), String> {
    let path = path.ok_or_else(|| {
        "no writable configuration path is available; correct the existing configuration file \
         and restart Kestrel"
            .to_owned()
    })?;
    save(path, configuration).map_err(|error| format!("{}: {error}", path.display()))
}

fn set_autostart_side_effect(enabled: bool) -> Result<(), String> {
    if enabled {
        enable_autostart().map_err(|error| error.to_string())
    } else {
        disable_autostart().map_err(|error| error.to_string())
    }
}

fn move_panel_section(
    sections: &mut [PanelSectionConfiguration],
    section: PanelSection,
    direction: PanelMoveDirection,
) -> bool {
    let Some(index) = sections.iter().position(|entry| entry.section == section) else {
        return false;
    };
    let target = match direction {
        PanelMoveDirection::Up => index.checked_sub(1),
        PanelMoveDirection::Down => (index + 1 < sections.len()).then_some(index + 1),
    };
    let Some(target) = target else { return false };
    sections.swap(index, target);
    true
}
fn move_monitor_readout(
    readouts: &mut [MonitorReadout],
    readout: MonitorReadout,
    direction: PanelMoveDirection,
) -> bool {
    let Some(index) = readouts.iter().position(|entry| *entry == readout) else {
        return false;
    };
    let target = match direction {
        PanelMoveDirection::Up => index.checked_sub(1),
        PanelMoveDirection::Down => (index + 1 < readouts.len()).then_some(index + 1),
    };
    let Some(target) = target else { return false };
    readouts.swap(index, target);
    true
}

fn panel_label(section: PanelSection) -> &'static str {
    match section {
        PanelSection::QuickControls => "Quick Controls",
        PanelSection::FeatureHub => "Feature Hub",
        PanelSection::Monitoring => "Monitoring",
    }
}

fn apply_color_scheme(preference: AppearancePreference) {
    let scheme = match preference {
        AppearancePreference::System => adw::ColorScheme::Default,
        AppearancePreference::Light => adw::ColorScheme::ForceLight,
        AppearancePreference::Dark => adw::ColorScheme::ForceDark,
    };
    adw::StyleManager::default().set_color_scheme(scheme);
}

fn load_startup_configuration(
    config_path: Option<PathBuf>,
) -> (LoadedConfiguration, Option<PathBuf>) {
    let Some(path) = config_path else {
        return (LoadedConfiguration::default(), None);
    };
    match load(&path) {
        Ok(loaded) => (loaded, Some(path)),
        Err(error) => {
            eprintln!("Kestrel could not load its configuration: {error}");
            let mut loaded = LoadedConfiguration::default();
            loaded.warnings.push(ConfigurationWarning {
                feature_id: "configuration".to_owned(),
                reason: format!(
                    "Configuration could not be loaded: {error}. \
                     Writes are disabled until the file is corrected and Kestrel is restarted."
                ),
            });
            (loaded, None)
        }
    }
}

fn main() {
    let (mut loaded, config_path) = load_startup_configuration(configuration_path());

    if loaded.configuration.startup.autostart {
        let result = thread::Builder::new()
            .name("kestrel-startup-autostart".to_owned())
            .spawn(setup_startup_autostart)
            .and_then(|worker| {
                worker
                    .join()
                    .map_err(|_| std::io::Error::other("worker panicked"))
            });
        if let Err(error) = result.and_then(|result| result) {
            loaded.configuration.startup.autostart = false;
            loaded.warnings.push(ConfigurationWarning {
                feature_id: "startup.autostart".to_owned(),
                reason: format!("Autostart could not be enabled at startup and was reset: {error}"),
            });
            if let Some(path) = config_path.as_deref() {
                let _ = save(path, &loaded.configuration);
            }
        }
    }

    let application = adw::Application::builder()
        .application_id("io.github.kamleshkc2002.Kestrel")
        .build();
    let initial_appearance = loaded.configuration.ui.appearance;
    let (commands, command_receiver) = async_channel::unbounded();
    let status_notifier = StatusNotifierIntegration::start(commands.clone());

    let mut runtime = ApplicationRuntime::new_with_status_notifier(
        &loaded.configuration,
        status_notifier.capability(),
    )
    .expect("built-in features have valid IDs");
    runtime.start();
    runtime
        .refresh_capabilities()
        .expect("built-in capability probes are internally consistent");

    let (refresh_results, refresh_receiver) = async_channel::unbounded();
    let (monitor_results, monitor_receiver) = async_channel::unbounded();
    let notifier: Arc<dyn AlertNotifier> = Arc::new(DesktopNotifier::new());
    let controller = ApplicationController::new(
        runtime,
        loaded.configuration,
        loaded.warnings,
        ControllerContext {
            config_path,
            refresh_results,
            monitor_results,
            commands: commands.clone(),
            notifier,
        },
    );
    install_command_actions(&application, commands);
    dispatch_commands(&application, &controller, command_receiver);
    dispatch_refresh_results(&controller, refresh_receiver);
    dispatch_monitor_results(&controller, monitor_receiver);
    let weak_monitor_controller = Rc::downgrade(&controller);
    glib::timeout_add_local(MONITOR_TICK_INTERVAL, move || {
        if let Some(controller) = weak_monitor_controller.upgrade() {
            controller.request_monitor_sample();
        }
        glib::ControlFlow::Continue
    });

    let weak_controller = Rc::downgrade(&controller);
    application.connect_startup(move |_| {
        apply_color_scheme(initial_appearance);
    });
    application.connect_activate(move |application| {
        if let Some(controller) = weak_controller.upgrade() {
            controller.present(application);
        }
    });

    application.run();
    drop(status_notifier);
}

fn setup_startup_autostart() -> Result<(), std::io::Error> {
    enable_autostart().map_err(|error| std::io::Error::other(error.to_string()))
}

fn install_command_actions(application: &adw::Application, commands: Sender<ApplicationCommand>) {
    add_command_action(
        application,
        "present-window",
        ApplicationCommand::PresentWindow,
        &commands,
    );
    add_command_action(
        application,
        "refresh-capabilities",
        ApplicationCommand::RefreshCapabilities,
        &commands,
    );
    add_command_action(application, "quit", ApplicationCommand::Quit, &commands);
    application.set_accels_for_action("app.refresh-capabilities", &["<Primary>r"]);
    application.set_accels_for_action("app.quit", &["<Primary>q"]);
}

fn add_command_action(
    application: &adw::Application,
    name: &str,
    command: ApplicationCommand,
    commands: &Sender<ApplicationCommand>,
) {
    let action = adw::gio::SimpleAction::new(name, None);
    let commands = commands.clone();
    action.connect_activate(move |_, _| {
        let _ = commands.try_send(command.clone());
    });
    application.add_action(&action);
}

fn dispatch_commands(
    application: &adw::Application,
    controller: &Rc<ApplicationController>,
    receiver: Receiver<ApplicationCommand>,
) {
    let application = application.clone();
    let controller = Rc::downgrade(controller);
    glib::spawn_future_local(async move {
        while let Ok(command) = receiver.recv().await {
            let Some(controller) = controller.upgrade() else {
                break;
            };
            match command {
                ApplicationCommand::PresentWindow => controller.present(&application),
                ApplicationCommand::RefreshCapabilities => controller.request_refresh(),
                ApplicationCommand::QuickToggle(command) => {
                    controller.request_quick_toggle(command)
                }
                ApplicationCommand::SetFeatureEnabled {
                    feature_id,
                    enabled,
                } => controller.request_operation(
                    ControllerOperation::SetFeatureEnabled {
                        feature_id,
                        enabled,
                    },
                    "kestrel-feature-change",
                ),
                ApplicationCommand::ApplyPreset(preset) => controller.request_operation(
                    ControllerOperation::ApplyPreset(preset),
                    "kestrel-preset-change",
                ),
                ApplicationCommand::UndoPreset => controller
                    .request_operation(ControllerOperation::UndoPreset, "kestrel-preset-undo"),
                ApplicationCommand::SetAppearance(appearance) => controller.request_operation(
                    ControllerOperation::SetAppearance(appearance),
                    "kestrel-appearance-change",
                ),
                ApplicationCommand::SetAutostart(enabled) => controller.request_operation(
                    ControllerOperation::SetAutostart(enabled),
                    "kestrel-autostart-change",
                ),
                ApplicationCommand::SetPanelVisibility { section, visible } => controller
                    .request_operation(
                        ControllerOperation::SetPanelVisibility { section, visible },
                        "kestrel-panel-visibility",
                    ),
                ApplicationCommand::MovePanelSection { section, direction } => controller
                    .request_operation(
                        ControllerOperation::MovePanelSection { section, direction },
                        "kestrel-panel-movement",
                    ),
                ApplicationCommand::SetMonitorReadoutVisible { readout, visible } => controller
                    .request_operation(
                        ControllerOperation::SetMonitorReadoutVisible { readout, visible },
                        "kestrel-monitor-readout-visibility",
                    ),
                ApplicationCommand::MoveMonitorReadout { readout, direction } => controller
                    .request_operation(
                        ControllerOperation::MoveMonitorReadout { readout, direction },
                        "kestrel-monitor-readout-movement",
                    ),
                ApplicationCommand::SetAlertEnabled { kind, enabled } => controller
                    .request_operation(
                        ControllerOperation::SetAlertEnabled { kind, enabled },
                        "kestrel-alert-enabled",
                    ),
                ApplicationCommand::SetAlertThreshold { kind, threshold } => controller
                    .request_operation(
                        ControllerOperation::SetAlertThreshold { kind, threshold },
                        "kestrel-alert-threshold",
                    ),
                ApplicationCommand::ImportConfiguration(path) => controller.request_operation(
                    ControllerOperation::ImportConfiguration(path),
                    "kestrel-configuration-import",
                ),
                ApplicationCommand::ExportConfiguration(path) => controller.request_operation(
                    ControllerOperation::ExportConfiguration(path),
                    "kestrel-configuration-export",
                ),
                ApplicationCommand::Quit => application.quit(),
            }
        }
    });
}

fn dispatch_refresh_results(
    controller: &Rc<ApplicationController>,
    receiver: Receiver<RefreshResult>,
) {
    let controller = Rc::downgrade(controller);
    glib::spawn_future_local(async move {
        while let Ok(result) = receiver.recv().await {
            let Some(controller) = controller.upgrade() else {
                break;
            };
            controller.finish_refresh(result);
        }
    });
}
fn dispatch_monitor_results(
    controller: &Rc<ApplicationController>,
    receiver: Receiver<MonitorResult>,
) {
    let controller = Rc::downgrade(controller);
    glib::spawn_future_local(async move {
        while let Ok(monitor) = receiver.recv().await {
            let Some(controller) = controller.upgrade() else {
                break;
            };
            controller.finish_monitor_sample(monitor);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{load_startup_configuration, move_monitor_readout, move_panel_section, persist};
    use kestrel::{MonitorReadout, PanelMoveDirection};
    use kestrel_core::{ApplicationConfiguration, PanelSection, PanelSectionConfiguration};

    #[test]
    fn panel_movement_is_bounded_and_deterministic() {
        let mut sections = vec![
            PanelSectionConfiguration {
                section: PanelSection::QuickControls,
                visible: true,
            },
            PanelSectionConfiguration {
                section: PanelSection::FeatureHub,
                visible: true,
            },
        ];
        assert!(!move_panel_section(
            &mut sections,
            PanelSection::QuickControls,
            PanelMoveDirection::Up
        ));
        assert!(move_panel_section(
            &mut sections,
            PanelSection::QuickControls,
            PanelMoveDirection::Down
        ));
        assert_eq!(sections[0].section, PanelSection::FeatureHub);
        assert_eq!(sections[1].section, PanelSection::QuickControls);
        assert!(!move_panel_section(
            &mut sections,
            PanelSection::QuickControls,
            PanelMoveDirection::Down
        ));
    }
    #[test]
    fn monitor_readout_movement_is_bounded_and_deterministic() {
        let mut readouts = vec![MonitorReadout::Cpu, MonitorReadout::Memory];
        assert!(!move_monitor_readout(
            &mut readouts,
            MonitorReadout::Cpu,
            PanelMoveDirection::Up
        ));
        assert!(move_monitor_readout(
            &mut readouts,
            MonitorReadout::Cpu,
            PanelMoveDirection::Down
        ));
        assert_eq!(readouts, vec![MonitorReadout::Memory, MonitorReadout::Cpu]);
        assert!(!move_monitor_readout(
            &mut readouts,
            MonitorReadout::Cpu,
            PanelMoveDirection::Down
        ));
    }

    #[test]
    fn invalid_startup_configuration_disables_persistence() {
        let path = std::env::temp_dir().join(format!(
            "kestrel-invalid-config-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_nanos()
        ));
        let original = b"[";
        std::fs::write(&path, original).expect("invalid fixture can be written");

        let (loaded, writable_path) = load_startup_configuration(Some(path.clone()));

        assert!(writable_path.is_none());
        assert!(loaded.warnings.iter().any(|warning| {
            warning.feature_id == "configuration" && warning.reason.contains("Writes are disabled")
        }));
        assert!(
            persist(
                writable_path.as_deref(),
                &ApplicationConfiguration::default()
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(&path).expect("invalid fixture remains readable"),
            original
        );

        std::fs::remove_file(path).expect("invalid fixture can be removed");
    }

    /// A disabled monitor feature must not acquire sampling resources: no worker thread
    /// is spawned and no result is published, so the acceptance criterion "uninstalled or
    /// disabled features acquire no runtime resources" is directly observable here. The
    /// enabled counterpart proves the same path still samples.
    #[test]
    fn monitor_ticks_are_inert_while_the_feature_is_disabled() {
        use super::{ApplicationController, ControllerContext, MonitorResult};
        use async_channel::Sender;
        use kestrel::ApplicationRuntime;
        use kestrel_platform::notifications::{
            AlertNotification, AlertNotifier, NotificationError,
        };
        use kestrel_platform::system_monitor::FEATURE_ID as SYSTEM_MONITOR_ID;
        use std::rc::Rc;
        use std::sync::Arc;

        struct SilentNotifier;

        impl AlertNotifier for SilentNotifier {
            fn notify(&self, _: &AlertNotification) -> Result<(), NotificationError> {
                Ok(())
            }
        }

        fn controller_for(
            configuration: &ApplicationConfiguration,
            monitor_results: Sender<MonitorResult>,
        ) -> Rc<ApplicationController> {
            let mut runtime =
                ApplicationRuntime::new(configuration).expect("built-in features have valid IDs");
            runtime.start();
            let (commands, _command_receiver) = async_channel::unbounded();
            let (refresh_results, _refresh_receiver) = async_channel::unbounded();
            ApplicationController::new(
                runtime,
                configuration.clone(),
                Vec::new(),
                ControllerContext {
                    config_path: None,
                    refresh_results,
                    monitor_results,
                    commands,
                    notifier: Arc::new(SilentNotifier),
                },
            )
        }

        let (disabled_results, disabled_receiver) = async_channel::unbounded();
        let disabled = controller_for(&ApplicationConfiguration::default(), disabled_results);
        disabled.request_monitor_sample();
        assert!(
            !disabled.monitoring.get(),
            "a disabled monitor must not start a sampling worker"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            disabled_receiver.try_recv().is_err(),
            "a disabled monitor must not publish a sample"
        );

        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(SYSTEM_MONITOR_ID, true)
            .expect("feature ID is valid");
        let (enabled_results, enabled_receiver) = async_channel::unbounded();
        let enabled = controller_for(&configuration, enabled_results);
        enabled.request_monitor_sample();
        assert!(
            enabled.monitoring.get(),
            "an enabled monitor samples on the next tick"
        );
        assert!(
            enabled_receiver.recv_blocking().is_ok(),
            "an enabled monitor publishes its tick result"
        );
    }
}
