mod window;

use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
    thread,
};

use adw::{glib, prelude::*};
use async_channel::{Receiver, Sender};
use kestrel::{
    AppearancePreference, ApplicationCommand, ApplicationRuntime, ApplicationViewModel,
    ConfigurationWarning, FeaturePreset, LoadedConfiguration, PanelMoveDirection, PanelSection,
    StatusNotifierIntegration, configuration_path, disable as disable_autostart,
    enable as enable_autostart, export_file, import_file, load, save,
};
use kestrel_core::{
    ApplicationConfiguration, FeatureConfigurationSnapshot, PanelSectionConfiguration,
};

use window::WindowView;

type RefreshResult = Result<(ApplicationViewModel, String), String>;

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
    ImportConfiguration(PathBuf),
    ExportConfiguration(PathBuf),
}

struct ApplicationController {
    state: SharedState,
    config_path: Option<PathBuf>,
    window: RefCell<Option<WindowView>>,
    refreshing: Cell<bool>,
    refresh_results: Sender<RefreshResult>,
    commands: Sender<ApplicationCommand>,
}

impl ApplicationController {
    fn new(
        runtime: ApplicationRuntime,
        configuration: ApplicationConfiguration,
        warnings: Vec<ConfigurationWarning>,
        config_path: Option<PathBuf>,
        refresh_results: Sender<RefreshResult>,
        commands: Sender<ApplicationCommand>,
    ) -> Rc<Self> {
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
            refresh_results,
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
                view.set_view_model(&view_model);
                view.show_message(&message);
            }
            Err(message) => {
                view.set_view_model(&self.current_view_model());
                view.show_message(&message);
            }
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

fn panel_label(section: PanelSection) -> &'static str {
    match section {
        PanelSection::QuickControls => "Quick Controls",
        PanelSection::FeatureHub => "Feature Hub",
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
    let controller = ApplicationController::new(
        runtime,
        loaded.configuration,
        loaded.warnings,
        config_path,
        refresh_results,
        commands.clone(),
    );
    install_command_actions(&application, commands);
    dispatch_commands(&application, &controller, command_receiver);
    dispatch_refresh_results(&controller, refresh_receiver);

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

#[cfg(test)]
mod tests {
    use super::{load_startup_configuration, move_panel_section, persist};
    use kestrel::PanelMoveDirection;
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
}
