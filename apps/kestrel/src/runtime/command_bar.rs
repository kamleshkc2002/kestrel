//! Command bar runtime features.

use super::*;

impl ApplicationRuntime {
    pub(super) fn scan_command_applications(
        configuration: &ApplicationConfiguration,
    ) -> Vec<ApplicationEntry> {
        if !configuration.command_bar.enable_applications {
            return Vec::new();
        }
        scan_applications(&application_directories(
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            std::env::var_os("XDG_DATA_DIRS").as_deref(),
            std::env::var_os("HOME").as_deref(),
        ))
    }

    pub(super) fn application_items(applications: &[ApplicationEntry]) -> Vec<CommandItem> {
        let mut items = Vec::new();
        for (index, application) in applications.iter().enumerate() {
            items.push(CommandItem::new(
                format!("application:{}", application.id),
                CommandSource::Application,
                application.name.clone(),
                application
                    .comment
                    .clone()
                    .unwrap_or_else(|| "Application".to_string()),
                kestrel_services::command_bar::CommandAction::OpenApplication { index },
            ));
            for action in &application.actions {
                items.push(CommandItem::new(
                    format!("app-action:{}:{}", application.id, action.id),
                    CommandSource::AppAction,
                    format!("{}: {}", application.name, action.name),
                    "Desktop application action",
                    kestrel_services::command_bar::CommandAction::OpenApplicationAction {
                        index,
                        action_id: action.id.clone(),
                    },
                ));
            }
        }
        items
    }

    pub(super) fn feature_items(registry: &FeatureRegistry) -> Vec<CommandItem> {
        registry
            .registrations()
            .map(|registration| {
                let enabled = registration.enabled;
                let label = registration.feature.label.to_string();
                CommandItem::new(
                    format!("feature:{}", registration.feature.id),
                    CommandSource::Feature,
                    format!("Turn {} {label}", if enabled { "off" } else { "on" }),
                    format!("Feature {}", registration.feature.id),
                    kestrel_services::command_bar::CommandAction::SetFeatureEnabled {
                        feature_id: registration.feature.id.to_owned(),
                        enabled: !enabled,
                    },
                )
                .with_keywords([
                    "feature",
                    registration.feature.id,
                    registration.feature.label,
                ])
            })
            .collect()
    }
    pub(super) fn snippet_items(library: &SnippetLibrary) -> Vec<CommandItem> {
        library
            .snippets()
            .iter()
            .map(|snippet| {
                CommandItem::new(
                    format!("snippet:{}", snippet.name),
                    CommandSource::Snippet,
                    snippet.name.clone(),
                    snippet
                        .folder
                        .clone()
                        .unwrap_or_else(|| "Snippet".to_string()),
                    kestrel_services::command_bar::CommandAction::Kestrel(format!(
                        "insert_snippet:{}",
                        snippet.name
                    )),
                )
                .with_keywords(["snippet", "text"])
            })
            .collect()
    }

    /// Enables file search only with a configured root.
    pub(super) fn providers_from_configuration(
        configuration: &ApplicationConfiguration,
    ) -> EnabledProviders {
        EnabledProviders {
            applications: configuration.command_bar.enable_applications,
            files: configuration.command_bar.enable_files
                && !configuration.command_bar.file_roots.is_empty(),
            scripts: configuration.command_bar.enable_scripts,
            emoji: configuration.command_bar.enable_emoji,
            windows: configuration.command_bar.enable_windows,
            clipboard: configuration.command_bar.enable_clipboard,
            // Snippets follow their own feature lifecycle.
            snippets: true,
        }
    }

    pub(super) fn rebuild_command_index(&mut self) {
        self.command_index = CommandIndex::new(
            Self::snippet_items(&self.snippet_library),
            &self.command_scripts,
        );
        self.command_application_items = Self::application_items(&self.command_applications);
        self.command_feature_items = Self::feature_items(&self.registry);
    }

    pub fn command_ranking(&self) -> &CommandRanking {
        &self.command_ranking
    }

    pub fn command_launcher_name(&self) -> Option<&str> {
        self.command_launcher
            .as_ref()
            .map(ExternalLauncher::handler_name)
    }

    pub fn command_bar_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == COMMAND_BAR_ID && registration.running)
    }

    /// Gates effects while stopped, keeping stale requests inactive.
    fn require_command_bar(&self) -> Result<(), String> {
        if self.command_bar_running() {
            Ok(())
        } else {
            Err("the command bar is not running; enable commands.bar in the Feature Hub".to_owned())
        }
    }

    /// Records the front window before the bar takes focus; compositors may
    /// stop reporting it as active while an overlay holds the keyboard.
    pub fn remember_front_window(&mut self) {
        let Some(provider) = self.window_provider.as_mut() else {
            return;
        };
        let Ok(windows) = provider.windows() else {
            return;
        };
        if let Some(front) = windows
            .iter()
            .find(|window| window.active && !is_kestrel_window(window.app_id.as_deref()))
        {
            self.last_external_active = Some(front.handle);
        }
    }

    fn window_items(&mut self) -> Vec<CommandItem> {
        let Some(provider) = self.window_provider.as_mut() else {
            return Vec::new();
        };
        let backend = provider.backend();
        let snapshot = match provider.windows() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                // The connection is not reused; a capability refresh reconnects.
                self.window_provider = None;
                self.window_provider_unavailable =
                    Some(kestrel_platform::windows::WindowProviderUnavailable {
                        summary: format!("The window list stopped responding: {error}"),
                        remediation: "Refresh capabilities to reconnect.".to_owned(),
                    });
                return Vec::new();
            }
        };
        let mut items = Vec::new();
        for window in snapshot {
            if is_kestrel_window(window.app_id.as_deref()) {
                continue;
            }
            if window.active {
                self.last_external_active = Some(window.handle);
            }
            let app = window
                .app_id
                .clone()
                .unwrap_or_else(|| "Unknown app".to_owned());
            items.push(CommandItem::new(
                format!("window:{}", window.handle.0),
                CommandSource::Window,
                window.title,
                format!("{app} · {}", backend.label()),
                kestrel_services::command_bar::CommandAction::ActivateWindow(window.handle.0),
            ));
        }
        items
    }

    fn clipboard_items(&self, query: &str) -> Vec<CommandItem> {
        let Some(Ok(matches)) = self.clipboard_search(query, 8) else {
            return Vec::new();
        };
        matches
            .into_iter()
            .map(|entry| {
                let preview = entry
                    .preview
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(160)
                    .collect::<String>();
                CommandItem::new(
                    format!("clipboard:{}", entry.id),
                    CommandSource::Clipboard,
                    preview,
                    format!("Clipboard entry {}", entry.id),
                    kestrel_services::command_bar::CommandAction::CopyClipboardEntry {
                        item_id: entry.id,
                    },
                )
            })
            .collect()
    }

    /// Returns no results when stopped; snippets follow their own lifecycle.
    pub fn command_search(
        &mut self,
        query: &str,
        now: &kestrel_platform::snippets::LocalTime,
    ) -> Vec<CommandResult> {
        if !self.command_bar_running() {
            return Vec::new();
        }
        let providers = EnabledProviders {
            snippets: self.snippets_running(),
            ..self.command_providers
        };
        let files = if self.command_providers.files {
            search_roots(
                &self.command_file_roots,
                query,
                kestrel_core::MAX_COMMAND_FILE_DEPTH,
                kestrel_core::MAX_COMMAND_FILE_ENTRIES,
                8,
            )
        } else {
            Vec::new()
        };
        let windows = if providers.windows {
            self.window_items()
        } else {
            Vec::new()
        };
        let clipboard = if providers.clipboard {
            self.clipboard_items(query)
        } else {
            Vec::new()
        };
        self.command_index.search(SearchInput {
            query,
            max_results: self.command_max_results,
            providers,
            applications: &self.command_application_items,
            files: &files,
            windows: &windows,
            clipboard: &clipboard,
            features: &self.command_feature_items,
            now: Some(now),
            ranking: &self.command_ranking,
            configured_scripts: &self.command_scripts,
        })
    }

    pub fn record_command_use(&mut self, id: &str) -> Result<(), String> {
        self.require_command_bar()?;
        self.command_ranking.record_use(id);
        self.persist_command_ranking()
    }

    pub fn set_command_pinned(&mut self, id: &str, pinned: bool) -> Result<(), String> {
        self.require_command_bar()?;
        self.command_ranking.set_pinned(id, pinned);
        self.persist_command_ranking()
    }

    pub fn reset_command_ranking(&mut self) -> Result<(), String> {
        self.require_command_bar()?;
        self.command_ranking.reset();
        self.persist_command_ranking()
    }

    fn persist_command_ranking(&self) -> Result<(), String> {
        let Some(path) = self.command_ranking_path.as_deref() else {
            return Err(
                "no writable ranking file is available; set XDG_DATA_HOME or HOME".to_owned(),
            );
        };
        crate::command_bar::save_ranking(path, &self.command_ranking)
    }

    /// Shared access prevents copying from disturbing history ownership.
    pub fn copy_to_clipboard(&mut self, text: &str) -> Result<(), String> {
        self.require_command_bar()?;
        let backend = self
            .session_clipboard
            .as_mut()
            .ok_or_else(|| "no clipboard connection is available for this session".to_string())?;
        backend.write_text(text).map_err(|error| error.to_string())
    }

    pub fn run_command_script(&self, index: usize) -> Result<ScriptOutcome, String> {
        self.require_command_bar()?;
        let script = self
            .command_scripts
            .get(index)
            .ok_or_else(|| format!("script {index} is not configured"))?;
        let path_env = std::env::var_os("PATH");
        run_script(script, path_env.as_deref()).map_err(|error| error.to_string())
    }

    pub fn activate_command_window(&mut self, handle: u64) -> Result<(), String> {
        self.require_command_bar()?;
        let provider = self
            .window_provider
            .as_mut()
            .ok_or_else(|| "window provider is unavailable".to_owned())?;
        let backend = provider.backend();
        provider
            .activate(WindowHandle(handle))
            .map_err(|error| match error.kind {
                WindowErrorKind::Unsupported => {
                    format!("{} cannot activate windows", backend.label())
                }
                _ => error.to_string(),
            })
    }

    pub fn launch_command_application_action(
        &self,
        index: usize,
        action_id: &str,
    ) -> Result<(), String> {
        self.require_command_bar()?;
        let application = self
            .command_applications
            .get(index)
            .ok_or_else(|| format!("application {index} is no longer available"))?;
        launch_desktop_action(application, action_id).map_err(|error| error.to_string())
    }

    /// Hides the application when it is in front, restores it when it has a
    /// window, and launches it otherwise.
    pub fn launch_command_application(
        &mut self,
        index: usize,
    ) -> Result<ApplicationWindowDecision, String> {
        self.require_command_bar()?;
        let application = self
            .command_applications
            .get(index)
            .cloned()
            .ok_or_else(|| format!("application {index} is no longer available"))?;
        // Without a readable window list the application is simply launched.
        let snapshot = self
            .window_provider
            .as_mut()
            .and_then(|provider| provider.windows().ok())
            .unwrap_or_default();
        let mut external = Vec::with_capacity(snapshot.len());
        for window in snapshot {
            if is_kestrel_window(window.app_id.as_deref()) {
                continue;
            }
            if window.active {
                self.last_external_active = Some(window.handle);
            }
            external.push(window);
        }
        let decision =
            decide_application_window_action(&external, self.last_external_active, &application);
        let provider = self.window_provider.as_mut();
        match (decision, provider) {
            (ApplicationWindowDecision::Hide(handle), Some(provider)) => {
                if !provider.backend().can_minimize() {
                    return Err(format!(
                        "{} cannot minimize windows",
                        provider.backend().label()
                    ));
                }
                provider
                    .minimize(handle)
                    .map_err(|error| error.to_string())?;
            }
            (ApplicationWindowDecision::Restore(handle), Some(provider)) => {
                provider
                    .activate(handle)
                    .map_err(|error| error.to_string())?;
            }
            _ => {
                launch_desktop_entry(&application).map_err(|error| error.to_string())?;
                return Ok(ApplicationWindowDecision::Launch);
            }
        }
        Ok(decision)
    }

    pub fn open_command_target(&self, target: &str) -> Result<(), String> {
        self.require_command_bar()?;
        let launcher = self
            .command_launcher
            .as_ref()
            .ok_or_else(|| "no desktop open handler is available".to_string())?;
        launcher.open(target).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::application;
    use super::*;
    use kestrel_core::Snippet;
    use kestrel_platform::snippets::SystemClock;
    use kestrel_services::command_bar::{CommandAction, CommandSource};
    #[test]
    fn a_disabled_command_bar_neither_searches_nor_acts() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();
        let now = SystemClock.local_time();

        assert!(!runtime.command_bar_running());
        runtime.command_ranking.set_pinned("kestrel:refresh", true);
        assert!(
            runtime.command_search("refresh", &now).is_empty(),
            "a stopped command bar ranks nothing"
        );
        for error in [
            runtime
                .run_command_script(0)
                .expect_err("scripts are gated"),
            runtime
                .launch_command_application(0)
                .expect_err("launching is gated"),
            runtime
                .open_command_target("https://example.com")
                .expect_err("opening is gated"),
            runtime
                .copy_to_clipboard("value")
                .expect_err("copying is gated"),
            runtime
                .set_command_pinned("kestrel:other", true)
                .expect_err("pinning is gated"),
            runtime
                .record_command_use("kestrel:other")
                .expect_err("recording a use is gated"),
            runtime
                .reset_command_ranking()
                .expect_err("resetting is gated"),
        ] {
            assert!(error.contains("not running"), "unexpected error: {error}");
        }
        assert!(
            runtime.command_ranking().is_pinned("kestrel:refresh"),
            "a refused reset leaves the seeded ranking untouched"
        );
        assert!(
            !runtime.command_ranking().is_pinned("kestrel:other"),
            "a refused pin adds nothing"
        );
    }
    #[test]
    fn an_enabled_command_bar_ranks_but_hides_snippets_while_they_are_stopped() {
        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(COMMAND_BAR_ID, true)
            .expect("feature ID is valid");
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");
        runtime.start();
        let now = SystemClock.local_time();
        assert!(runtime.command_bar_running());
        assert!(!runtime.snippets_running());

        runtime
            .snippet_library
            .upsert(
                Snippet::new("Zq Greeting", None, None, "hello"),
                runtime.snippet_policy,
            )
            .expect("snippet is valid");
        runtime.rebuild_command_index();

        assert!(
            runtime
                .command_search("refresh", &now)
                .iter()
                .any(|result| result.item.id == "kestrel:refresh"),
            "portable commands answer while the bar runs"
        );
        assert!(
            runtime
                .command_search("zq greeting", &now)
                .iter()
                .all(|result| result.item.source != CommandSource::Snippet),
            "a stopped snippet feature offers nothing to run"
        );
    }
    #[test]
    fn applications_that_need_a_terminal_are_offered_and_launched_through_their_desktop_file() {
        let entries = vec![
            application("top.desktop", "Top", true),
            application("editor.desktop", "Editor", false),
        ];

        let items = ApplicationRuntime::application_items(&entries);

        assert_eq!(
            items.len(),
            2,
            "terminal entries are offered like any other"
        );
        assert_eq!(
            items[0].action,
            CommandAction::OpenApplication { index: 0 },
            "items address the scanned list"
        );

        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(COMMAND_BAR_ID, true)
            .expect("feature ID is valid");
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");
        runtime.start();
        // Entries need desktop files for GIO launching.
        runtime.command_applications = entries;
        let error = runtime
            .launch_command_application(0)
            .expect_err("an entry without a desktop file cannot launch");
        assert!(error.contains("no desktop file"), "{error}");
        assert!(
            runtime
                .launch_command_application(9)
                .expect_err("unknown index")
                .contains("no longer available")
        );
    }
}
