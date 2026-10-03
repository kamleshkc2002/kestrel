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
use kestrel::view_model::{ClipboardQuery, SnippetQuery};
use kestrel::{
    AlertKind, AppearancePreference, ApplicationCommand, ApplicationRuntime, ApplicationViewModel,
    ClipboardLifecycle, ClipboardViewModel, ConfigurationWarning, FeaturePreset,
    LoadedConfiguration, MonitorReadout, MonitorViewModel, PanelMoveDirection, PanelSection,
    SnippetCommand, SnippetDraft, SnippetLimit, SnippetMatch, SnippetProviderPreference,
    StatusNotifierIntegration, configuration_path, disable as disable_autostart,
    enable as enable_autostart, export_file, import_file, load, save,
};
use kestrel_core::{
    ApplicationConfiguration, FeatureConfigurationSnapshot, PanelSectionConfiguration,
};
use kestrel_platform::notifications::{AlertNotification, AlertNotifier, DesktopNotifier};
use kestrel_services::alerts::notification_text;

use window::WindowView;

const TICK_INTERVAL: Duration = Duration::from_millis(250);

type RefreshResult = Result<(ApplicationViewModel, String), String>;
/// A periodic tick: each field is `None` when that feature produced no new state.
type TickUpdate = (Option<MonitorViewModel>, Option<ClipboardViewModel>);

type SharedState = Arc<Mutex<ControllerState>>;

/// The entries one search request may return to the window.
const CLIPBOARD_SEARCH_LIMIT: usize = 50;
/// The largest preview the window may request for one entry.
const CLIPBOARD_PREVIEW_BYTES: usize = 2048;
/// The most snippets one search request returns.
const SNIPPET_SEARCH_LIMIT: usize = 100;

struct ControllerState {
    runtime: ApplicationRuntime,
    configuration: ApplicationConfiguration,
    warnings: Vec<ConfigurationWarning>,
    preset_snapshot: Option<FeatureConfigurationSnapshot>,
    /// The application's current clipboard query and its bounded results.
    clipboard_query: String,
    clipboard_matches: Vec<kestrel_services::clipboard::ClipboardMatch>,
    clipboard_preview: Option<kestrel_services::clipboard::ClipboardPreview>,
    /// The last clipboard state delivered to the window.
    clipboard_fingerprint: Option<ClipboardFingerprint>,
    /// The application's snippet query, its results, and the open editor.
    snippet_query: String,
    snippet_matches: Vec<SnippetMatch>,
    snippet_draft: Option<SnippetDraft>,
}

impl ControllerState {
    fn view_model(&self) -> ApplicationViewModel {
        self.runtime.view_model_with_panels(
            &self.warnings,
            &self.configuration,
            self.preset_snapshot.is_some(),
            ClipboardQuery {
                query: &self.clipboard_query,
                matches: &self.clipboard_matches,
                preview: self.clipboard_preview.as_ref(),
            },
            SnippetQuery {
                query: &self.snippet_query,
                matches: &self.snippet_matches,
                draft: self.snippet_draft.as_ref(),
            },
        )
    }

    /// Re-runs the snippet search so the list reflects a library change.
    fn refresh_snippet_matches(&mut self) {
        self.snippet_matches = self
            .runtime
            .snippet_matches(&self.snippet_query, SNIPPET_SEARCH_LIMIT);
    }

    /// Re-runs the current clipboard query so the list reflects a mutation.
    fn refresh_clipboard_matches(&mut self) {
        self.clipboard_preview = None;
        match self
            .runtime
            .clipboard_search(&self.clipboard_query, CLIPBOARD_SEARCH_LIMIT)
        {
            Some(Ok(matches)) => self.clipboard_matches = matches,
            Some(Err(_)) | None => self.clipboard_matches.clear(),
        }
    }
}

/// Samples the monitor, delivers alerts, and reports clipboard changes.
///
/// The state mutex is held only for the sample, for recording delivery results,
/// and for one clipboard comparison; never across the blocking notification call.
fn run_tick(
    state: &SharedState,
    notifier: &dyn AlertNotifier,
    observed_at: Duration,
) -> TickUpdate {
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
    let monitor = updated.then(|| deliver_alerts(state, notifier, alerts));
    let clipboard = clipboard_tick_update(state);
    (monitor, clipboard)
}

/// Delivers alert notifications outside the controller lock, then reports the
/// resulting monitor presentation state.
fn deliver_alerts(
    state: &SharedState,
    notifier: &dyn AlertNotifier,
    alerts: Vec<kestrel_services::alerts::AlertEvent>,
) -> MonitorViewModel {
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
    guard.runtime.monitor_view_model(&guard.configuration)
}

/// Builds clipboard presentation state when the worker's snapshot changed.
fn clipboard_tick_update(state: &SharedState) -> Option<ClipboardViewModel> {
    let mut guard = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let snapshot = guard.runtime.clipboard_snapshot();
    let fingerprint = ClipboardFingerprint::of(&snapshot);
    if guard.clipboard_fingerprint == Some(fingerprint) {
        return None;
    }
    guard.clipboard_fingerprint = Some(fingerprint);
    // The row list is produced by an explicit query, so re-run the current one
    // when the snapshot changed: a background capture must appear in the list,
    // not only in the retained count.
    let refreshed = {
        let query = &guard.clipboard_query;
        guard
            .runtime
            .clipboard_search(query, CLIPBOARD_SEARCH_LIMIT)
    };
    if let Some(Ok(matches)) = refreshed {
        guard.clipboard_matches = matches;
    }
    if let Some(preview) = guard.clipboard_preview.as_ref() {
        if !guard
            .clipboard_matches
            .iter()
            .any(|matched| matched.id == preview.id)
        {
            guard.clipboard_preview = None;
        }
    }
    Some(guard.runtime.clipboard_view_model(
        &guard.configuration,
        &guard.clipboard_query,
        &guard.clipboard_matches,
        guard.clipboard_preview.as_ref(),
    ))
}

/// Which part of the window a completed operation refreshes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TargetedPanel {
    #[default]
    Full,
    Clipboard,
    Snippets,
}

/// Cheap change detector for the clipboard snapshot.
///
/// The worker publishes on every poll, so the tick compares this fingerprint
/// instead of rebuilding presentation state (or touching widgets) each time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClipboardFingerprint {
    lifecycle: ClipboardLifecycle,
    items: usize,
    total_bytes: usize,
    pinned: usize,
    rejected: u64,
    filtered: u64,
    clears: u64,
    wipes: u64,
    has_error: bool,
}

impl ClipboardFingerprint {
    fn of(snapshot: &kestrel_services::clipboard::ClipboardSnapshot) -> Self {
        Self {
            lifecycle: snapshot.lifecycle,
            items: snapshot.items.len(),
            total_bytes: snapshot.total_bytes,
            pinned: snapshot.pinned_items,
            rejected: snapshot.rejected_oversize_items,
            filtered: snapshot.filtered_sensitive_items,
            clears: snapshot.selection_clears,
            wipes: snapshot.wipe_count,
            has_error: snapshot.last_error.is_some(),
        }
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
    Audio(kestrel::AudioCommand),
    SetAudioBoostPercent(u8),
    SetAudioOutputSwitch(kestrel::AudioOutputSwitch),
    SetAudioDisconnectPolicy(kestrel::AudioDisconnectPolicy),
    SetAudioDisconnectVolumePercent(u8),
    SetAudioIncludeInactiveStreams(bool),
    Clipboard(kestrel::ClipboardCommand),
    ClipboardSearch(String),
    ClipboardPreview(u64),
    SetClipboardLimit(kestrel::ClipboardLimit),
    SetClipboardFilterSensitive(bool),
    SetClipboardPastePlainText(bool),
    Snippet(SnippetCommand),
    SnippetSearch(String),
    SnippetEdit(String),
    SnippetNew,
    SetSnippetLimit(SnippetLimit),
    SetSnippetProvider(SnippetProviderPreference),
    SetSnippetExpansionTiming(kestrel::SnippetExpansionTiming),
    ImportConfiguration(PathBuf),
    ExportConfiguration(PathBuf),
}

/// Channels, paths, and adapters that the application loop hands to the controller.
struct ControllerContext {
    config_path: Option<PathBuf>,
    refresh_results: Sender<RefreshResult>,
    tick_results: Sender<TickUpdate>,
    commands: Sender<ApplicationCommand>,
    notifier: Arc<dyn AlertNotifier>,
}

struct ApplicationController {
    state: SharedState,
    config_path: Option<PathBuf>,
    window: RefCell<Option<WindowView>>,
    refreshing: Cell<bool>,
    /// Which panel a queued clipboard or snippet operation updates in place.
    targeted: Cell<TargetedPanel>,
    monitoring: Cell<bool>,
    /// Mirrors the monitor registration so a disabled feature never spawns tick workers.
    monitor_running: Cell<bool>,
    /// Mirrors the clipboard registration so background captures reach the window.
    clipboard_running: Cell<bool>,
    monitor_started_at: Instant,
    notifier: Arc<dyn AlertNotifier>,
    refresh_results: Sender<RefreshResult>,
    tick_results: Sender<TickUpdate>,
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
            tick_results,
            commands,
            notifier,
        } = context;
        let monitor_running = runtime.monitor_is_running();
        let clipboard_running = runtime.clipboard_is_running();
        Rc::new(Self {
            state: Arc::new(Mutex::new(ControllerState {
                runtime,
                configuration,
                warnings,
                preset_snapshot: None,
                clipboard_query: String::new(),
                clipboard_matches: Vec::new(),
                clipboard_preview: None,
                clipboard_fingerprint: None,
                snippet_query: String::new(),
                snippet_matches: Vec::new(),
                snippet_draft: None,
            })),
            config_path,
            window: RefCell::new(None),
            refreshing: Cell::new(false),
            targeted: Cell::new(TargetedPanel::Full),
            monitoring: Cell::new(false),
            monitor_running: Cell::new(monitor_running),
            clipboard_running: Cell::new(clipboard_running),
            monitor_started_at: Instant::now(),
            notifier,
            refresh_results,
            tick_results,
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

    /// Queues a clipboard operation that updates only the clipboard group.
    fn request_clipboard_operation(
        &self,
        operation: ControllerOperation,
        worker_name: &'static str,
    ) {
        self.request_targeted_operation(operation, worker_name, TargetedPanel::Clipboard);
    }

    /// Queues a snippet operation that updates only the snippet group.
    fn request_snippet_operation(&self, operation: ControllerOperation, worker_name: &'static str) {
        self.request_targeted_operation(operation, worker_name, TargetedPanel::Snippets);
    }

    /// Queues an operation that updates one panel instead of the whole page.
    ///
    /// Those panels own their search fields, so a full page rebuild while the
    /// user types would steal focus and lose the query.
    fn request_targeted_operation(
        &self,
        operation: ControllerOperation,
        worker_name: &'static str,
        panel: TargetedPanel,
    ) {
        // The single-operation gate is checked first so a rejected request
        // never leaves the targeted-update state set.
        if self.refreshing.get() {
            self.request_operation(operation, worker_name);
            return;
        }
        self.targeted.set(panel);
        self.request_operation(operation, worker_name);
    }

    fn finish_refresh(&self, result: RefreshResult) {
        self.refreshing.set(false);
        let window = self.window.borrow();
        let Some(view) = window.as_ref() else {
            return;
        };
        view.set_refreshing(false);
        let targeted = self.targeted.replace(TargetedPanel::Full);
        match result {
            Ok((view_model, message)) => {
                apply_color_scheme(view_model.appearance);
                self.monitor_running.set(view_model.monitor.running);
                self.clipboard_running.set(view_model.clipboard.running);
                apply_targeted_panel(view, &view_model, targeted);
                if !message.is_empty() {
                    view.show_message(&message);
                }
            }
            Err(message) => {
                let view_model = self.current_view_model();
                self.monitor_running.set(view_model.monitor.running);
                self.clipboard_running.set(view_model.clipboard.running);
                apply_targeted_panel(view, &view_model, targeted);
                view.show_message(&message);
            }
        }
    }
    fn request_tick(&self) {
        if self.refreshing.get() || self.monitoring.replace(true) {
            return;
        }
        // The tick serves the monitor and the clipboard panel, so it runs while
        // either feature is live.
        if !self.monitor_running.get() && !self.clipboard_running.get() {
            self.monitoring.set(false);
            return;
        }
        let state = Arc::clone(&self.state);
        let notifier = Arc::clone(&self.notifier);
        let results = self.tick_results.clone();
        let observed_at = self.monitor_started_at.elapsed();
        let worker = thread::Builder::new()
            .name("kestrel-tick".to_owned())
            .spawn(move || {
                let update = run_tick(&state, notifier.as_ref(), observed_at);
                let _ = results.send_blocking(update);
            });
        if worker.is_err() {
            self.monitoring.set(false);
        }
    }

    fn finish_tick(&self, update: TickUpdate) {
        self.monitoring.set(false);
        let (monitor, clipboard) = update;
        let window = self.window.borrow();
        let Some(view) = window.as_ref() else {
            return;
        };
        if let Some(monitor) = monitor {
            self.monitor_running.set(monitor.running);
            view.set_monitor(&monitor);
        }
        if let Some(clipboard) = clipboard {
            self.clipboard_running.set(clipboard.running);
            view.set_clipboard(&clipboard);
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
                self.commit_configuration(
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
                self.commit_configuration(
                    candidate,
                    config_path,
                    "Monitor readout order updated".to_owned(),
                )?
            }
            ControllerOperation::SetAlertEnabled { kind, enabled } => {
                let mut candidate = self.configuration.clone();
                candidate.monitoring.alerts.rule_mut(kind).enabled = enabled;
                self.commit_configuration(
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
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("{} alert threshold updated", kind.label()),
                )?
            }
            ControllerOperation::Audio(command) => {
                let summary = audio_command_summary(command);
                match self.runtime.execute_audio_command(command) {
                    Some(Ok(_)) => format!("{summary} updated"),
                    Some(Err(failure)) => format!("{summary} failed: {}", failure.error),
                    None => "The audio mixer is not running; enable audio.mixer first".to_owned(),
                }
            }
            ControllerOperation::SetAudioBoostPercent(percent) => {
                let mut candidate = self.configuration.clone();
                candidate.audio.boost_percent = percent;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("Audio boost ceiling set to {percent}%"),
                )?
            }
            ControllerOperation::SetAudioOutputSwitch(mode) => {
                let mut candidate = self.configuration.clone();
                candidate.audio.output_switch = mode;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("Output switching set to {}", mode.label()),
                )?
            }
            ControllerOperation::SetAudioDisconnectPolicy(policy) => {
                let mut candidate = self.configuration.clone();
                candidate.audio.disconnect_policy = policy;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("Disconnect policy set to {}", policy.label()),
                )?
            }
            ControllerOperation::SetAudioDisconnectVolumePercent(percent) => {
                let mut candidate = self.configuration.clone();
                candidate.audio.disconnect_volume_percent = percent;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("Disconnect volume set to {percent}%"),
                )?
            }
            ControllerOperation::SetAudioIncludeInactiveStreams(include) => {
                let mut candidate = self.configuration.clone();
                candidate.audio.include_inactive_streams = include;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!(
                        "Inactive streams {}",
                        if include { "shown" } else { "hidden" }
                    ),
                )?
            }
            ControllerOperation::Clipboard(command) => {
                let summary = clipboard_command_summary(&command);
                match self.runtime.execute_clipboard_command(command) {
                    Some(Ok(snapshot)) => {
                        self.refresh_clipboard_matches();
                        format!("{summary} · {} entries retained", snapshot.items.len())
                    }
                    Some(Err(error)) => format!("{summary} failed: {error}"),
                    None => "Clipboard history is not running; enable clipboard.history first"
                        .to_owned(),
                }
            }
            ControllerOperation::ClipboardSearch(query) => {
                self.clipboard_query = query.clone();
                self.clipboard_preview = None;
                match self
                    .runtime
                    .clipboard_search(&query, CLIPBOARD_SEARCH_LIMIT)
                {
                    Some(Ok(matches)) => {
                        let shown = matches.len();
                        self.clipboard_matches = matches;
                        if query.trim().is_empty() {
                            // An empty query is the initial listing; it updates
                            // the list without a toast.
                            String::new()
                        } else {
                            format!("{shown} entries match \"{query}\"")
                        }
                    }
                    Some(Err(error)) => {
                        self.clipboard_matches.clear();
                        format!("Clipboard search failed: {error}")
                    }
                    None => {
                        self.clipboard_matches.clear();
                        "Clipboard history is not running".to_owned()
                    }
                }
            }
            ControllerOperation::ClipboardPreview(item_id) => {
                self.clipboard_preview = None;
                match self
                    .runtime
                    .clipboard_preview(item_id, CLIPBOARD_PREVIEW_BYTES)
                {
                    Some(Ok(preview)) => {
                        let truncated = if preview.truncated {
                            " (truncated)"
                        } else {
                            ""
                        };
                        self.clipboard_preview = Some(preview);
                        format!("Entry preview loaded{truncated}")
                    }
                    Some(Err(error)) => format!("Entry preview failed: {error}"),
                    None => "Clipboard history is not running".to_owned(),
                }
            }
            ControllerOperation::SetClipboardLimit(limit) => {
                let mut candidate = self.configuration.clone();
                let message = match limit {
                    kestrel::ClipboardLimit::Items(items) => {
                        candidate.clipboard.max_items = items;
                        format!("Clipboard retains at most {items} entries")
                    }
                    kestrel::ClipboardLimit::ItemBytes(bytes) => {
                        candidate.clipboard.max_item_bytes = bytes;
                        format!("Clipboard text entries are bounded to {bytes} bytes")
                    }
                    kestrel::ClipboardLimit::ImageBytes(bytes) => {
                        candidate.clipboard.max_image_bytes = bytes;
                        format!("Clipboard image entries are bounded to {bytes} bytes")
                    }
                    kestrel::ClipboardLimit::FileEntries(entries) => {
                        candidate.clipboard.max_file_entries = entries;
                        format!("Clipboard file entries are bounded to {entries} paths")
                    }
                    kestrel::ClipboardLimit::MaxAgeHours(hours) => {
                        candidate.clipboard.max_age_hours = hours;
                        format!("Clipboard entries expire after {hours} hours")
                    }
                    kestrel::ClipboardLimit::ClearSeconds(seconds) => {
                        candidate.clipboard.clear_seconds = seconds;
                        if seconds == 0 {
                            "Automatic selection clearing is disabled".to_owned()
                        } else {
                            format!("The live selection is cleared after {seconds} seconds")
                        }
                    }
                };
                self.commit_configuration(candidate, config_path, message)?
            }
            ControllerOperation::SetClipboardFilterSensitive(filter) => {
                let mut candidate = self.configuration.clone();
                candidate.clipboard.filter_sensitive = filter;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!(
                        "Sensitive-pattern filtering {}",
                        if filter { "enabled" } else { "disabled" }
                    ),
                )?
            }
            ControllerOperation::SetClipboardPastePlainText(plain) => {
                let mut candidate = self.configuration.clone();
                candidate.clipboard.paste_plain_text = plain;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!(
                        "Quick paste copies {}",
                        if plain {
                            "plain text"
                        } else {
                            "the original form"
                        }
                    ),
                )?
            }
            ControllerOperation::Snippet(command) => match command {
                SnippetCommand::Insert(name) => match self.runtime.insert_snippet(&name) {
                    Ok(report) => {
                        let mut message = format!(
                            "Inserted \"{name}\" ({} bytes via {})",
                            report.bytes,
                            report.provider.label()
                        );
                        if report.clipboard_truncated {
                            message.push_str(" · clipboard variable clipped to the bound");
                        }
                        if report.clipboard_empty {
                            message.push_str(" · the live selection was empty");
                        }
                        message
                    }
                    Err(error) => format!("Insert failed: {error}"),
                },
                SnippetCommand::Save {
                    name,
                    folder,
                    trigger,
                    content,
                } => {
                    let snippet = kestrel_core::Snippet::new(
                        name.trim().to_string(),
                        optional_field(folder),
                        optional_field(trigger),
                        content,
                    );
                    let label = snippet.name.clone();
                    match self.runtime.save_snippet(snippet) {
                        Ok(()) => {
                            self.refresh_snippet_matches();
                            format!("Snippet \"{label}\" saved")
                        }
                        Err(error) => format!("Snippet was not saved: {error}"),
                    }
                }
                SnippetCommand::Delete(name) => match self.runtime.delete_snippet(&name) {
                    Ok(()) => {
                        self.refresh_snippet_matches();
                        if self
                            .snippet_draft
                            .as_ref()
                            .is_some_and(|draft| draft.name == name)
                        {
                            self.snippet_draft = None;
                        }
                        format!("Snippet \"{name}\" deleted")
                    }
                    Err(error) => format!("Snippet was not deleted: {error}"),
                },
            },
            ControllerOperation::SnippetSearch(query) => {
                self.snippet_query = query.clone();
                self.refresh_snippet_matches();
                if query.trim().is_empty() {
                    // The window shows a count in its status line, so the initial
                    // listing does not need a toast.
                    String::new()
                } else {
                    format!("{} snippets match \"{query}\"", self.snippet_matches.len())
                }
            }
            ControllerOperation::SnippetEdit(name) => {
                match self.runtime.snippet_library().get(&name) {
                    Some(snippet) => {
                        self.snippet_draft = Some(SnippetDraft {
                            name: snippet.name.clone(),
                            folder: snippet.folder.clone().unwrap_or_default(),
                            trigger: snippet.trigger.clone().unwrap_or_default(),
                            content: snippet.content.clone(),
                        });
                        format!("Editing \"{name}\"")
                    }
                    None => format!("No snippet named \"{name}\" is stored"),
                }
            }
            ControllerOperation::SnippetNew => {
                self.snippet_draft = Some(SnippetDraft::default());
                "New snippet editor opened".to_owned()
            }
            ControllerOperation::SetSnippetLimit(limit) => {
                let mut candidate = self.configuration.clone();
                let message = match limit {
                    SnippetLimit::ContentBytes(bytes) => {
                        candidate.snippets.max_content_bytes = bytes;
                        format!("Snippet content is bounded to {bytes} bytes")
                    }
                    SnippetLimit::ClipboardBytes(bytes) => {
                        candidate.snippets.clipboard_variable_bytes = bytes;
                        format!("The clipboard variable is bounded to {bytes} bytes")
                    }
                    SnippetLimit::InsertTimeoutMillis(millis) => {
                        candidate.snippets.insert_timeout_millis = millis;
                        format!("Insertion times out after {millis} ms")
                    }
                };
                self.commit_configuration(candidate, config_path, message)?
            }
            ControllerOperation::SetSnippetProvider(preference) => {
                let mut candidate = self.configuration.clone();
                candidate.snippets.preferred_provider = preference;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("Insertion provider preference: {}", preference.label()),
                )?
            }
            ControllerOperation::SetSnippetExpansionTiming(timing) => {
                let mut candidate = self.configuration.clone();
                candidate.snippets.expansion_timing = timing;
                self.commit_configuration(
                    candidate,
                    config_path,
                    format!("Expansion timing: {}", timing.label()),
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
    fn commit_configuration(
        &mut self,
        candidate: ApplicationConfiguration,
        config_path: Option<&Path>,
        message: String,
    ) -> Result<String, String> {
        candidate
            .validate()
            .map_err(|error| format!("Invalid configuration: {error:?}"))?;
        let old = self.configuration.clone();
        self.runtime
            .apply_configuration(&candidate)
            .map_err(|error| {
                self.restore_configuration(old.clone());
                format!("Could not apply the configuration: {error:?}")
            })?;
        if let Err(error) = persist(config_path, &candidate) {
            self.restore_configuration(old);
            return Err(format!("Configuration change was not saved: {error}"));
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

/// Names the mixer operation that a status message refers to.
fn audio_command_summary(command: kestrel::AudioCommand) -> &'static str {
    match command {
        kestrel::AudioCommand::SetStreamVolume { .. } => "Stream volume",
        kestrel::AudioCommand::SetStreamMute { .. } => "Stream mute",
        kestrel::AudioCommand::MoveStream { .. } => "Stream routing",
        kestrel::AudioCommand::SetOutputVolume { .. } => "Output volume",
        kestrel::AudioCommand::SetOutputMute { .. } => "Output mute",
        kestrel::AudioCommand::SetDefaultOutput { .. } => "Default output",
        kestrel::AudioCommand::CycleOutput { .. } => "Output cycle",
    }
}

/// Trims a snippet field and treats an empty value as absent.
fn optional_field(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Applies a completed operation to the whole page or to one panel.
///
/// The clipboard and snippet panels own their search fields, so refreshing them
/// in place keeps the query and the focus while the user types.
fn apply_targeted_panel(
    view: &WindowView,
    view_model: &ApplicationViewModel,
    targeted: TargetedPanel,
) {
    match targeted {
        TargetedPanel::Full => view.set_view_model(view_model),
        TargetedPanel::Clipboard => view.set_clipboard(&view_model.clipboard),
        TargetedPanel::Snippets => view.set_snippets(&view_model.snippets),
    }
}

/// Names the clipboard operation that a status message refers to.
fn clipboard_command_summary(command: &kestrel::ClipboardCommand) -> &'static str {
    match command {
        kestrel::ClipboardCommand::Refresh => "Clipboard refresh",
        kestrel::ClipboardCommand::CopyItem { .. } => "Clipboard copy",
        kestrel::ClipboardCommand::DeleteItems { .. } => "Clipboard delete",
        kestrel::ClipboardCommand::SetPinned { .. } => "Clipboard pin",
        kestrel::ClipboardCommand::ReplaceText { .. } => "Clipboard edit",
        kestrel::ClipboardCommand::ClearSelection => "Clipboard selection clear",
        kestrel::ClipboardCommand::Wipe => "Clipboard wipe",
    }
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
    let (tick_results, tick_receiver) = async_channel::unbounded();
    let notifier: Arc<dyn AlertNotifier> = Arc::new(DesktopNotifier::new());
    let controller = ApplicationController::new(
        runtime,
        loaded.configuration,
        loaded.warnings,
        ControllerContext {
            config_path,
            refresh_results,
            tick_results,
            commands: commands.clone(),
            notifier,
        },
    );
    install_command_actions(&application, commands);
    dispatch_commands(&application, &controller, command_receiver);
    dispatch_refresh_results(&controller, refresh_receiver);
    dispatch_tick_results(&controller, tick_receiver);
    let weak_monitor_controller = Rc::downgrade(&controller);
    glib::timeout_add_local(TICK_INTERVAL, move || {
        if let Some(controller) = weak_monitor_controller.upgrade() {
            controller.request_tick();
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
                ApplicationCommand::Audio(command) => controller
                    .request_operation(ControllerOperation::Audio(command), "kestrel-audio"),
                ApplicationCommand::SetAudioBoostPercent(percent) => controller.request_operation(
                    ControllerOperation::SetAudioBoostPercent(percent),
                    "kestrel-audio-boost",
                ),
                ApplicationCommand::SetAudioOutputSwitch(mode) => controller.request_operation(
                    ControllerOperation::SetAudioOutputSwitch(mode),
                    "kestrel-audio-output-switch",
                ),
                ApplicationCommand::SetAudioDisconnectPolicy(policy) => controller
                    .request_operation(
                        ControllerOperation::SetAudioDisconnectPolicy(policy),
                        "kestrel-audio-disconnect",
                    ),
                ApplicationCommand::SetAudioDisconnectVolumePercent(percent) => controller
                    .request_operation(
                        ControllerOperation::SetAudioDisconnectVolumePercent(percent),
                        "kestrel-audio-disconnect-volume",
                    ),
                ApplicationCommand::SetAudioIncludeInactiveStreams(include) => controller
                    .request_operation(
                        ControllerOperation::SetAudioIncludeInactiveStreams(include),
                        "kestrel-audio-inactive",
                    ),
                ApplicationCommand::Clipboard(command) => controller.request_clipboard_operation(
                    ControllerOperation::Clipboard(command),
                    "kestrel-clipboard",
                ),
                ApplicationCommand::ClipboardSearch(query) => controller
                    .request_clipboard_operation(
                        ControllerOperation::ClipboardSearch(query),
                        "kestrel-clipboard-search",
                    ),
                ApplicationCommand::ClipboardPreview(item_id) => controller
                    .request_clipboard_operation(
                        ControllerOperation::ClipboardPreview(item_id),
                        "kestrel-clipboard-preview",
                    ),
                ApplicationCommand::SetClipboardLimit(limit) => controller.request_operation(
                    ControllerOperation::SetClipboardLimit(limit),
                    "kestrel-clipboard-limit",
                ),
                ApplicationCommand::SetClipboardFilterSensitive(filter) => controller
                    .request_operation(
                        ControllerOperation::SetClipboardFilterSensitive(filter),
                        "kestrel-clipboard-filter",
                    ),
                ApplicationCommand::SetClipboardPastePlainText(plain) => controller
                    .request_operation(
                        ControllerOperation::SetClipboardPastePlainText(plain),
                        "kestrel-clipboard-paste",
                    ),
                ApplicationCommand::Snippet(command) => controller.request_snippet_operation(
                    ControllerOperation::Snippet(command),
                    "kestrel-snippet",
                ),
                ApplicationCommand::SnippetSearch(query) => controller.request_snippet_operation(
                    ControllerOperation::SnippetSearch(query),
                    "kestrel-snippet-search",
                ),
                ApplicationCommand::SnippetEdit(name) => controller.request_snippet_operation(
                    ControllerOperation::SnippetEdit(name),
                    "kestrel-snippet-edit",
                ),
                ApplicationCommand::SnippetNew => controller.request_snippet_operation(
                    ControllerOperation::SnippetNew,
                    "kestrel-snippet-new",
                ),
                ApplicationCommand::SetSnippetLimit(limit) => controller.request_operation(
                    ControllerOperation::SetSnippetLimit(limit),
                    "kestrel-snippet-limit",
                ),
                ApplicationCommand::SetSnippetProvider(preference) => controller.request_operation(
                    ControllerOperation::SetSnippetProvider(preference),
                    "kestrel-snippet-provider",
                ),
                ApplicationCommand::SetSnippetExpansionTiming(timing) => controller
                    .request_operation(
                        ControllerOperation::SetSnippetExpansionTiming(timing),
                        "kestrel-snippet-expansion",
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
fn dispatch_tick_results(controller: &Rc<ApplicationController>, receiver: Receiver<TickUpdate>) {
    let controller = Rc::downgrade(controller);
    glib::spawn_future_local(async move {
        while let Ok(monitor) = receiver.recv().await {
            let Some(controller) = controller.upgrade() else {
                break;
            };
            controller.finish_tick(monitor);
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
        use super::{ApplicationController, ControllerContext};
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
            tick_results: Sender<super::TickUpdate>,
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
                    tick_results,
                    commands,
                    notifier: Arc::new(SilentNotifier),
                },
            )
        }

        let (disabled_results, disabled_receiver) = async_channel::unbounded();
        let disabled = controller_for(&ApplicationConfiguration::default(), disabled_results);
        disabled.request_tick();
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
        enabled.request_tick();
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
