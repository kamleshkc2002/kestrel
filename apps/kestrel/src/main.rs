mod window;

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{Arc, Mutex},
    thread,
};

use adw::{glib, prelude::*};
use async_channel::{Receiver, Sender};
use kestrel::{
    ApplicationCommand, ApplicationRuntime, ApplicationViewModel, ConfigurationWarning,
    LoadedConfiguration, StatusNotifierIntegration, configuration_path, load,
};

use window::WindowView;

type RefreshResult = Result<(ApplicationViewModel, String), String>;

struct ApplicationController {
    runtime: Arc<Mutex<ApplicationRuntime>>,
    warnings: Arc<Vec<ConfigurationWarning>>,
    window: RefCell<Option<WindowView>>,
    refreshing: Cell<bool>,
    refresh_results: Sender<RefreshResult>,
    commands: Sender<ApplicationCommand>,
}

impl ApplicationController {
    fn new(
        runtime: ApplicationRuntime,
        warnings: Vec<ConfigurationWarning>,
        refresh_results: Sender<RefreshResult>,
        commands: Sender<ApplicationCommand>,
    ) -> Rc<Self> {
        Rc::new(Self {
            runtime: Arc::new(Mutex::new(runtime)),
            warnings: Arc::new(warnings),
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
        if self.refreshing.replace(true) {
            return;
        }
        if let Some(view) = self.window.borrow().as_ref() {
            view.set_refreshing(true);
        }

        let runtime = Arc::clone(&self.runtime);
        let warnings = Arc::clone(&self.warnings);
        let results = self.refresh_results.clone();
        let worker = thread::Builder::new()
            .name("kestrel-capability-refresh".to_owned())
            .spawn(move || {
                let mut runtime = runtime
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let result = runtime
                    .refresh_capabilities()
                    .map(|()| {
                        (
                            runtime.view_model(&warnings),
                            "Capability and toggle state refreshed".to_owned(),
                        )
                    })
                    .map_err(|error| format!("Capability refresh failed: {error:?}"));
                let _ = results.send_blocking(result);
            });

        if let Err(error) = worker {
            self.finish_refresh(Err(format!(
                "Could not start the capability refresh worker: {error}"
            )));
        }
    }

    fn request_quick_toggle(&self, command: kestrel::QuickToggleCommand) {
        if self.refreshing.replace(true) {
            if let Some(view) = self.window.borrow().as_ref() {
                view.show_message("Another Kestrel operation is still running");
            }
            return;
        }
        if let Some(view) = self.window.borrow().as_ref() {
            view.set_refreshing(true);
        }
        let runtime = Arc::clone(&self.runtime);
        let warnings = Arc::clone(&self.warnings);
        let results = self.refresh_results.clone();
        let label = command.id.label();
        let worker = thread::Builder::new()
            .name("kestrel-quick-toggle".to_owned())
            .spawn(move || {
                let mut runtime = runtime
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let result = match runtime.execute_quick_toggle_command(command) {
                    Some(Ok(_)) => Ok((runtime.view_model(&warnings), format!("{label} updated"))),
                    Some(Err(error)) => Ok((
                        runtime.view_model(&warnings),
                        format!("{label} failed: {error}"),
                    )),
                    None => Ok((
                        runtime.view_model(&warnings),
                        format!("{label} is not available"),
                    )),
                };
                let _ = results.send_blocking(result);
            });
        if let Err(error) = worker {
            self.finish_refresh(Err(format!(
                "Could not start the quick-toggle worker: {error}"
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
                view.set_view_model(&view_model);
                view.show_message(&message);
            }
            Err(message) => view.show_message(&message),
        }
    }

    fn current_view_model(&self) -> ApplicationViewModel {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .view_model(&self.warnings)
    }
}

fn main() {
    let loaded = configuration_path()
        .as_deref()
        .map(load)
        .transpose()
        .unwrap_or_else(|error| {
            eprintln!("Kestrel could not load its configuration: {error}");
            Some(LoadedConfiguration::default())
        })
        .unwrap_or_default();

    let application = adw::Application::builder()
        .application_id("io.github.kamleshkc2002.Kestrel")
        .build();
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
    let controller =
        ApplicationController::new(runtime, loaded.warnings, refresh_results, commands.clone());
    install_command_actions(&application, commands);
    dispatch_commands(&application, &controller, command_receiver);
    dispatch_refresh_results(&controller, refresh_receiver);

    let weak_controller = Rc::downgrade(&controller);
    application.connect_activate(move |application| {
        if let Some(controller) = weak_controller.upgrade() {
            controller.present(application);
        }
    });

    application.run();
    drop(status_notifier);
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
