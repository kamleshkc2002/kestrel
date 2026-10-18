//! Snippet library and insertion runtime features.

use super::*;

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
    pub fn save_snippet(&mut self, snippet: kestrel_core::Snippet) -> Result<(), String> {
        let mut candidate = self.snippet_library.clone();
        candidate
            .upsert(snippet, self.snippet_policy)
            .map_err(|error| error.to_string())?;
        self.persist_snippets(&candidate)?;
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

    /// Reads the clipboard once, clips it to the configured bound, and requires
    /// the snippet feature to be running before typing into another application.
    pub fn insert_snippet(&mut self, name: &str) -> Result<InsertionReport, SnippetServiceError> {
        if !self.snippets_running() {
            return Err(SnippetServiceError::ExpansionUnavailable {
                reason: self
                    .snippets_stopped_reason()
                    .unwrap_or_else(|| crate::view_model::SNIPPETS_OFF.to_owned()),
            });
        }
        let snippet = self.snippet_library.get(name).cloned().ok_or_else(|| {
            SnippetServiceError::ExpansionUnavailable {
                reason: format!("no snippet named \"{name}\" is stored"),
            }
        })?;
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
        self.snippet_service
            .insert(&snippet, self.snippet_policy, &context)
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
            .insert_snippet("Greeting")
            .expect_err("a stopped feature must not type text");

        assert!(matches!(
            &error,
            SnippetServiceError::ExpansionUnavailable { reason }
                if reason.contains("Enable snippets.text")
        ));
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
