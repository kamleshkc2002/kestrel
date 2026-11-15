//! Snippet library and insertion runtime features.

use super::*;

/// Why a snippet was not stored: the definition was rejected, or the private
/// file could not be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnippetSaveError {
    Invalid(kestrel_core::SnippetError),
    Storage(String),
}

impl SnippetSaveError {
    pub fn message(&self) -> String {
        match self {
            Self::Invalid(error) => error.to_string(),
            Self::Storage(reason) => reason.clone(),
        }
    }
}

impl ApplicationRuntime {
    pub(super) fn build_snippet_service(
        configuration: &ApplicationConfiguration,
    ) -> SnippetInsertionService<ExecutableInsertionBackend> {
        let discovery = discover_insertion_provider(configuration.snippets.preferred_provider)
            .and_then(|path| {
                ExecutableInsertionBackend::new(path, configuration.snippets.insert_timeout_millis)
            });
        SnippetInsertionService::new(discovery)
    }

    /// Separate read-only access avoids disturbing history ownership.
    pub(super) fn build_snippet_clipboard() -> Option<ArboardClipboardBackend> {
        discover_provider()
            .ok()
            .and_then(|provider| ArboardClipboardBackend::new(provider).ok())
    }

    pub fn snippet_library(&self) -> &SnippetLibrary {
        &self.snippet_library
    }

    pub fn snippet_policy(&self) -> SnippetPolicy {
        self.snippet_policy
    }

    pub fn snippets_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == SNIPPETS_ID && registration.running)
    }

    /// Why snippets are not running: switched off, or the capability's remedy.
    pub(super) fn snippets_stopped_reason(&self) -> Option<String> {
        let registration = self
            .registry
            .registrations()
            .find(|registration| registration.feature.id == SNIPPETS_ID)?;
        if registration.running {
            return None;
        }
        Some(if registration.enabled {
            registration
                .capability
                .remediation
                .clone()
                .unwrap_or_else(|| registration.capability.summary.clone())
        } else {
            crate::view_model::SNIPPETS_OFF.to_owned()
        })
    }

    pub fn snippet_provider(&self) -> Option<InsertionProvider> {
        self.snippet_service.provider()
    }

    pub fn snippet_unavailable_reason(&self) -> Option<&str> {
        self.snippet_service.unavailable_reason()
    }

    pub fn snippet_directory(&self) -> Option<String> {
        self.snippet_path
            .as_deref()
            .and_then(|path| path.parent())
            .map(|parent| parent.display().to_string())
    }

    pub fn snippet_matches(&self, query: &str, limit: usize) -> Vec<SnippetMatch> {
        self.snippet_library
            .search(query, limit, self.snippet_policy)
    }

    /// Writes a candidate before replacing live state, so failed saves stay inert.
    pub fn save_snippet(&mut self, snippet: kestrel_core::Snippet) -> Result<(), SnippetSaveError> {
        let mut candidate = self.snippet_library.clone();
        candidate
            .upsert(snippet, self.snippet_policy)
            .map_err(SnippetSaveError::Invalid)?;
        self.persist_snippets(&candidate)
            .map_err(SnippetSaveError::Storage)?;
        self.snippet_library = candidate;
        self.rebuild_command_index();
        Ok(())
    }

    /// Removal takes effect only after persistence succeeds.
    pub fn delete_snippet(&mut self, name: &str) -> Result<(), String> {
        let mut candidate = self.snippet_library.clone();
        candidate.remove(name).map_err(|error| error.to_string())?;
        self.persist_snippets(&candidate)?;
        self.snippet_library = candidate;
        self.rebuild_command_index();
        Ok(())
    }

    fn persist_snippets(&self, library: &SnippetLibrary) -> Result<(), String> {
        let Some(path) = self.snippet_path.as_deref() else {
            return Err(
                "no writable snippet file is available; set XDG_DATA_HOME or HOME".to_owned(),
            );
        };
        save_snippets(path, library, self.snippet_policy).map_err(|error| error.to_string())
    }

    /// Starts one insertion after the shared modifier and focus wait; nothing is
    /// typed until the caller observes the focus leave.
    pub fn begin_snippet_insert(&mut self, name: &str) -> Result<(), SnippetServiceError> {
        if !self.snippets_running() {
            return Err(SnippetServiceError::ExpansionUnavailable {
                reason: self
                    .snippets_stopped_reason()
                    .unwrap_or_else(|| crate::view_model::SNIPPETS_OFF.to_owned()),
            });
        }
        if self.snippet_service.provider().is_none() {
            return Err(SnippetServiceError::ExpansionUnavailable {
                reason: self
                    .snippet_service
                    .unavailable_reason()
                    .unwrap_or("no verified insertion provider")
                    .to_owned(),
            });
        }
        if self.snippet_insertion_active() {
            return Err(SnippetServiceError::ExpansionUnavailable {
                reason: "another insertion is still running".to_owned(),
            });
        }
        let snippet = self.snippet_library.get(name).cloned().ok_or_else(|| {
            SnippetServiceError::ExpansionUnavailable {
                reason: format!("no snippet named \"{name}\" is stored"),
            }
        })?;
        self.snippet_insertion = Some(SnippetInsertionOperation::new(
            snippet,
            self.snippet_policy,
            std::time::Instant::now(),
        ));
        Ok(())
    }

    pub fn snippet_insertion_active(&self) -> bool {
        self.snippet_insertion
            .as_ref()
            .is_some_and(|operation| !operation.is_terminal())
    }

    pub fn snippet_insertion_stage(&self) -> Option<SnippetInsertionStage> {
        self.snippet_insertion
            .as_ref()
            .map(|operation| operation.stage().clone())
    }

    /// Advances the wait; returns the new stage and keeps `Ready` stable until
    /// the sender runs.
    pub fn observe_snippet_insert(
        &mut self,
        observation: kestrel_services::paste::PasteObservation,
    ) -> Option<SnippetInsertionStage> {
        let operation = self.snippet_insertion.as_mut()?;
        if operation.is_terminal() {
            return Some(operation.stage().clone());
        }
        Some(
            operation
                .advance(std::time::Instant::now(), observation)
                .clone(),
        )
    }

    /// Ends a pending insertion when snippets stop; the observer then brings
    /// Kestrel back with the reason.
    pub(super) fn cancel_snippet_insert(&mut self) {
        if let Some(operation) = self.snippet_insertion.as_mut() {
            operation.cancel();
        }
    }

    /// Renders against the live context and types once, only in `Ready` and
    /// only while snippets still run.
    pub fn send_snippet_insert(
        &mut self,
    ) -> Option<Result<(String, InsertionReport), SnippetServiceError>> {
        if !self.snippets_running() {
            self.cancel_snippet_insert();
            return None;
        }
        let operation = self.snippet_insertion.as_mut()?;
        if !operation.stage().is_ready() {
            return None;
        }
        let name = operation.snippet().name.clone();
        let clipboard_available = self.session_clipboard.is_some();
        let clipboard = self
            .session_clipboard
            .as_mut()
            .and_then(|backend| backend.read_text().ok().flatten());
        let clock = SystemClock;
        let local_time = clock.local_time();
        let timezone = clock.timezone_name();
        let context = RenderContext {
            local_time: Some(local_time),
            timezone: Some(timezone.as_str()),
            clipboard: clipboard.as_deref(),
            clipboard_available,
        };
        // A configuration change can drop the provider while the handoff waits.
        let Some(backend) = self.snippet_service.backend_mut() else {
            let reason = self
                .snippet_service
                .unavailable_reason()
                .unwrap_or("no verified insertion provider")
                .to_owned();
            operation.fail(kestrel_services::paste::PasteFailure::new(
                None,
                reason.clone(),
            ));
            return Some(Err(SnippetServiceError::ExpansionUnavailable { reason }));
        };
        Some(
            operation
                .send(&context, backend)
                .expect("the caller checked the ready stage")
                .map(|report| (name, report)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_core::Snippet;
    use kestrel_services::snippets::{SnippetLibrary, SnippetServiceError};
    use tempfile::TempDir;
    #[test]
    fn snippet_insertion_requires_the_running_feature() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();
        runtime
            .snippet_library
            .upsert(
                Snippet::new("Greeting", None, None, "hello"),
                runtime.snippet_policy,
            )
            .expect("snippet is valid");

        let error = runtime
            .begin_snippet_insert("Greeting")
            .expect_err("a stopped feature must not start an insertion");

        assert!(matches!(
            &error,
            SnippetServiceError::ExpansionUnavailable { reason }
                if reason.contains("Enable snippets.text")
        ));
    }
    #[test]
    fn a_pending_insertion_ends_without_typing_when_snippets_stop_or_lose_their_provider() {
        use kestrel_core::SnippetProviderPreference;
        use kestrel_platform::snippets::{
            ExecutableInsertionBackend, GraphicalSession, discover_insertion_provider_with,
        };
        use kestrel_services::paste::PasteObservation;
        use kestrel_services::snippets::{SnippetInsertionService, SnippetInsertionStage};
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().expect("temp dir");
        let typed = dir.path().join("typed");
        let provider = dir.path().join("wtype");
        std::fs::write(
            &provider,
            format!("#!/bin/sh\ncat > '{}'\n", typed.display()),
        )
        .expect("fake provider is written");
        std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755))
            .expect("fake provider is executable");
        let path = discover_insertion_provider_with(
            SnippetProviderPreference::Wtype,
            Some(dir.path().as_os_str()),
            Some(GraphicalSession::Wayland),
        )
        .expect("the fake provider is found");

        let mut configuration = ApplicationConfiguration::default();
        configuration
            .set_feature_enabled(SNIPPETS_ID, true)
            .expect("feature ID is valid");
        let mut runtime = ApplicationRuntime::new(&configuration).expect("runtime builds");
        runtime.start();
        assert!(runtime.snippets_running());
        runtime.snippet_service =
            SnippetInsertionService::new(ExecutableInsertionBackend::new(path.clone(), 2_000));
        runtime
            .snippet_library
            .upsert(
                Snippet::new("Greeting", None, None, "hello"),
                runtime.snippet_policy,
            )
            .expect("snippet is valid");

        // Modifiers are still held when the feature is switched off.
        runtime
            .begin_snippet_insert("Greeting")
            .expect("the insertion starts");
        runtime
            .apply_configuration(&ApplicationConfiguration::default())
            .expect("snippets turn off");
        assert!(!runtime.snippets_running());

        let released = PasteObservation {
            focus_released: true,
            modifiers_released: true,
        };
        assert_eq!(
            runtime.observe_snippet_insert(released),
            Some(SnippetInsertionStage::Cancelled)
        );
        assert!(runtime.send_snippet_insert().is_none());
        assert!(!runtime.snippet_insertion_active());
        assert!(!typed.exists(), "the provider was never run");

        // A configuration change drops the provider after the handoff is ready.
        runtime
            .apply_configuration(&configuration)
            .expect("snippets turn back on");
        runtime.snippet_service =
            SnippetInsertionService::new(ExecutableInsertionBackend::new(path, 2_000));
        runtime
            .begin_snippet_insert("Greeting")
            .expect("the insertion starts");
        let mut stage = None;
        for _ in 0..3 {
            stage = runtime.observe_snippet_insert(released);
        }
        assert_eq!(stage, Some(SnippetInsertionStage::Ready));
        let empty = TempDir::new().expect("empty PATH dir");
        runtime.snippet_service = SnippetInsertionService::new(
            discover_insertion_provider_with(
                SnippetProviderPreference::Auto,
                Some(empty.path().as_os_str()),
                Some(GraphicalSession::Wayland),
            )
            .and_then(|path| ExecutableInsertionBackend::new(path, 2_000)),
        );

        assert!(matches!(
            runtime.send_snippet_insert(),
            Some(Err(SnippetServiceError::ExpansionUnavailable { .. }))
        ));
        assert!(matches!(
            runtime.snippet_insertion_stage(),
            Some(SnippetInsertionStage::Failed(_))
        ));
        assert!(!runtime.snippet_insertion_active());
        assert!(!typed.exists(), "the provider was never run");
    }
    #[test]
    fn a_failed_snippet_write_leaves_the_library_unchanged() {
        let dir = TempDir::new().expect("temp dir");
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "a regular file").expect("blocker is written");
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.snippet_library = SnippetLibrary::default();
        runtime
            .snippet_library
            .upsert(
                Snippet::new("Keep", None, None, "kept"),
                runtime.snippet_policy,
            )
            .expect("snippet is valid");
        // A regular-file parent makes writes fail.
        runtime.snippet_path = Some(blocker.join("snippets.toml"));

        runtime
            .save_snippet(Snippet::new("Rejected", None, None, "nope"))
            .expect_err("the write fails");
        assert!(
            runtime.snippet_library().get("Rejected").is_none(),
            "a snippet that was not saved must not stay active"
        );

        runtime.delete_snippet("Keep").expect_err("the write fails");
        assert!(
            runtime.snippet_library().get("Keep").is_some(),
            "a snippet that was not deleted must stay"
        );

        let path = dir.path().join("data/snippets.toml");
        runtime.snippet_path = Some(path.clone());
        runtime
            .save_snippet(Snippet::new("Greeting", None, None, "hello"))
            .expect("the save succeeds");
        let reloaded = crate::load_snippets(&path, runtime.snippet_policy());
        assert!(reloaded.warnings.is_empty());
        assert_eq!(reloaded.library.names(), vec!["Keep", "Greeting"]);

        runtime.delete_snippet("Keep").expect("the delete succeeds");
        assert!(runtime.snippet_library().get("Keep").is_none());
        assert_eq!(
            crate::load_snippets(&path, runtime.snippet_policy())
                .library
                .names(),
            vec!["Greeting"]
        );
    }
}
