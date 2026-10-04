//! Deterministic snippet rendering with capability-gated insertion.
//!
//! Expansion uses supplied context and bounded clipboard text; insertion requires
//! a verified provider.

use std::{error::Error, fmt, time::Duration};

use kestrel_core::{Snippet, SnippetError, SnippetVariable};
use kestrel_platform::snippets::{InsertionBackend, InsertionError, InsertionProvider, LocalTime};

pub const CLIPBOARD_PLACEHOLDER: &str = "«clipboard»";
pub const CLIPBOARD_EMPTY: &str = "«clipboard empty»";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnippetPolicy {
    pub max_content_bytes: u32,
    pub clipboard_variable_bytes: u32,
    pub insert_timeout: Duration,
}

impl Default for SnippetPolicy {
    fn default() -> Self {
        Self::from_configuration(&kestrel_core::SnippetConfiguration::default())
    }
}

impl SnippetPolicy {
    pub fn from_configuration(configuration: &kestrel_core::SnippetConfiguration) -> Self {
        Self {
            max_content_bytes: configuration.max_content_bytes,
            clipboard_variable_bytes: configuration.clipboard_variable_bytes,
            insert_timeout: Duration::from_millis(configuration.insert_timeout_millis),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderContext<'a> {
    pub local_time: Option<LocalTime>,
    pub timezone: Option<&'a str>,
    /// Caller-provided live selection; `None` with availability means empty.
    pub clipboard: Option<&'a str>,
    pub clipboard_available: bool,
}

impl RenderContext<'_> {
    pub fn preview() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedSnippet {
    pub text: String,
    pub variables: Vec<SnippetVariable>,
    pub clipboard_truncated: bool,
    pub clipboard_empty: bool,
}

/// Expands variables locally, clipping clipboard text to the configured bound.
pub fn render(
    snippet: &Snippet,
    policy: SnippetPolicy,
    context: &RenderContext<'_>,
) -> RenderedSnippet {
    let mut text = String::with_capacity(snippet.content.len());
    let mut variables = Vec::new();
    let mut clipboard_truncated = false;
    let mut clipboard_empty = false;

    let mut rest = snippet.content.as_str();
    while let Some(start) = rest.find("{{") {
        let after = &rest[start..];
        let Some(end) = after.find("}}") else {
            break;
        };
        text.push_str(&rest[..start]);
        let token = &after[..end + 2];
        match SnippetVariable::parse(token) {
            Some(SnippetVariable::Clipboard) => {
                if !variables.contains(&SnippetVariable::Clipboard) {
                    variables.push(SnippetVariable::Clipboard);
                }
                match (context.clipboard, context.clipboard_available) {
                    (Some(value), _) if !value.is_empty() => {
                        let (clipped, truncated) =
                            clip_text(value, policy.clipboard_variable_bytes as usize);
                        clipboard_truncated |= truncated;
                        text.push_str(&clipped);
                    }
                    (_, true) => {
                        clipboard_empty = true;
                        text.push_str(CLIPBOARD_EMPTY);
                    }
                    (_, false) => text.push_str(CLIPBOARD_PLACEHOLDER),
                }
            }
            Some(variable) => {
                if !variables.contains(&variable) {
                    variables.push(variable);
                }
                let value = render_variable(variable, context);
                text.push_str(&value);
            }
            None => text.push_str(token),
        }
        rest = &after[end + 2..];
    }
    text.push_str(rest);

    RenderedSnippet {
        text,
        variables,
        clipboard_truncated,
        clipboard_empty,
    }
}

fn render_variable(variable: SnippetVariable, context: &RenderContext<'_>) -> String {
    let Some(time) = context.local_time else {
        return format!("«{}»", variable.token().trim_matches(['{', '}']));
    };
    match variable {
        SnippetVariable::Date => time.iso_date(),
        SnippetVariable::Time => time.iso_time(),
        SnippetVariable::DateTime => time.iso_datetime(),
        SnippetVariable::UtcOffset => time.utc_offset(),
        SnippetVariable::Timezone => context
            .timezone
            .map(str::to_owned)
            .unwrap_or_else(|| "local".to_string()),
        SnippetVariable::Clipboard => CLIPBOARD_PLACEHOLDER.to_string(),
    }
}

/// Clips text to a byte bound on a character boundary.
pub fn clip_text(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetMatch {
    pub name: String,
    pub folder: Option<String>,
    pub trigger: Option<String>,
    pub preview: String,
    pub preview_truncated: bool,
    pub variables: Vec<SnippetVariable>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnippetLibrary {
    snippets: Vec<Snippet>,
}

impl SnippetLibrary {
    pub fn from_snippets(snippets: Vec<Snippet>) -> Self {
        Self { snippets }
    }

    pub fn snippets(&self) -> &[Snippet] {
        &self.snippets
    }

    pub fn len(&self) -> usize {
        self.snippets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.snippets.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&Snippet> {
        self.snippets.iter().find(|snippet| snippet.name == name)
    }

    pub fn names(&self) -> Vec<String> {
        self.snippets
            .iter()
            .map(|snippet| snippet.name.clone())
            .collect()
    }

    /// Folders in first-appearance order, followed by ungrouped snippets.
    pub fn folders(&self) -> Vec<String> {
        let mut folders: Vec<String> = Vec::new();
        for snippet in &self.snippets {
            if let Some(folder) = snippet.folder.as_deref() {
                if !folders.iter().any(|existing| existing == folder) {
                    folders.push(folder.to_string());
                }
            }
        }
        folders
    }

    /// Adds or replaces a snippet, enforcing unique names and delimited triggers.
    pub fn upsert(&mut self, snippet: Snippet, policy: SnippetPolicy) -> Result<(), SnippetError> {
        snippet.validate(policy.max_content_bytes)?;
        if let Some(trigger) = snippet.trigger.as_deref().map(str::trim) {
            let first = trigger.chars().next().unwrap_or_default();
            if first.is_alphanumeric() {
                return Err(SnippetError::TriggerNotDelimited {
                    trigger: trigger.to_owned(),
                });
            }
            let conflict = self.snippets.iter().any(|existing| {
                existing.name != snippet.name
                    && existing
                        .trigger
                        .as_deref()
                        .is_some_and(|other| other.eq_ignore_ascii_case(trigger))
            });
            if conflict {
                return Err(SnippetError::DuplicateTrigger {
                    trigger: trigger.to_owned(),
                });
            }
        }
        match self
            .snippets
            .iter_mut()
            .find(|existing| existing.name == snippet.name)
        {
            Some(existing) => *existing = snippet,
            None => {
                if self
                    .snippets
                    .iter()
                    .any(|existing| existing.name.eq_ignore_ascii_case(&snippet.name))
                {
                    return Err(SnippetError::DuplicateName {
                        name: snippet.name.clone(),
                    });
                }
                self.snippets.push(snippet);
            }
        }
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<Snippet, SnippetError> {
        let index = self
            .snippets
            .iter()
            .position(|snippet| snippet.name == name)
            .ok_or_else(|| SnippetError::UnknownSnippet {
                name: name.to_string(),
            })?;
        Ok(self.snippets.remove(index))
    }

    pub fn search(&self, query: &str, limit: usize, policy: SnippetPolicy) -> Vec<SnippetMatch> {
        let needle = query.trim().to_lowercase();
        let limit = limit.clamp(1, 100);
        self.snippets
            .iter()
            .filter(|snippet| {
                needle.is_empty()
                    || snippet.name.to_lowercase().contains(&needle)
                    || snippet
                        .folder
                        .as_deref()
                        .is_some_and(|folder| folder.to_lowercase().contains(&needle))
                    || snippet
                        .trigger
                        .as_deref()
                        .is_some_and(|trigger| trigger.to_lowercase().contains(&needle))
                    || snippet.content.to_lowercase().contains(&needle)
            })
            .take(limit)
            .map(|snippet| {
                let rendered = render(snippet, policy, &RenderContext::preview());
                let (preview, truncated) = clip_text(&rendered.text, 240);
                SnippetMatch {
                    name: snippet.name.clone(),
                    folder: snippet.folder.clone(),
                    trigger: snippet.trigger.clone(),
                    preview: preview.replace(['\n', '\r'], " "),
                    preview_truncated: truncated,
                    variables: rendered.variables,
                }
            })
            .collect()
    }
}

impl SnippetLibrary {
    pub fn validate(&self, policy: SnippetPolicy) -> Result<(), SnippetError> {
        let mut checked = SnippetLibrary::default();
        for snippet in &self.snippets {
            checked.upsert(snippet.clone(), policy)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertionReport {
    pub provider: InsertionProvider,
    pub bytes: usize,
    pub clipboard_truncated: bool,
    pub clipboard_empty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnippetServiceError {
    /// Insertion stays disabled until a verified provider is available.
    ExpansionUnavailable {
        reason: String,
    },
    Insertion(InsertionError),
}

impl fmt::Display for SnippetServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExpansionUnavailable { reason } => {
                write!(formatter, "snippet insertion is disabled: {reason}")
            }
            Self::Insertion(error) => error.fmt(formatter),
        }
    }
}

impl Error for SnippetServiceError {}

/// Owns insertion; action requires a verified provider.
pub struct SnippetInsertionService<B: InsertionBackend> {
    backend: Option<B>,
    unavailable: Option<String>,
}

impl<B: InsertionBackend> SnippetInsertionService<B> {
    /// Builds the service, preserving why provider discovery failed.
    pub fn new(backend: Result<B, InsertionError>) -> Self {
        match backend {
            Ok(backend) => Self {
                backend: Some(backend),
                unavailable: None,
            },
            Err(error) => Self {
                backend: None,
                unavailable: Some(error.message),
            },
        }
    }

    pub fn available(&self) -> bool {
        self.backend.is_some()
    }

    pub fn provider(&self) -> Option<InsertionProvider> {
        self.backend.as_ref().map(InsertionBackend::provider)
    }

    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    /// Renders and inserts one snippet when a provider is available.
    pub fn insert(
        &mut self,
        snippet: &Snippet,
        policy: SnippetPolicy,
        context: &RenderContext<'_>,
    ) -> Result<InsertionReport, SnippetServiceError> {
        let Some(backend) = self.backend.as_mut() else {
            return Err(SnippetServiceError::ExpansionUnavailable {
                reason: self
                    .unavailable
                    .clone()
                    .unwrap_or_else(|| "no verified insertion provider".to_string()),
            });
        };
        let rendered = render(snippet, policy, context);
        let bytes = rendered.text.len();
        backend
            .insert_text(&rendered.text)
            .map_err(SnippetServiceError::Insertion)?;
        Ok(InsertionReport {
            provider: backend.provider(),
            bytes,
            clipboard_truncated: rendered.clipboard_truncated,
            clipboard_empty: rendered.clipboard_empty,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use kestrel_core::{Snippet, SnippetError, SnippetProviderPreference, SnippetVariable};
    use kestrel_platform::snippets::{
        InsertionBackend, InsertionError, InsertionErrorKind, InsertionProvider, LocalTime,
    };

    use super::{
        CLIPBOARD_EMPTY, CLIPBOARD_PLACEHOLDER, RenderContext, SnippetInsertionService,
        SnippetLibrary, SnippetPolicy, SnippetServiceError, clip_text, render,
    };

    const TIME: LocalTime = LocalTime {
        year: 2026,
        month: 10,
        day: 2,
        hour: 9,
        minute: 5,
        second: 7,
        utc_offset_seconds: 7200,
    };

    #[derive(Clone, Default)]
    struct FakeBackend {
        inserted: Arc<Mutex<Vec<String>>>,
        fail: bool,
    }

    impl InsertionBackend for FakeBackend {
        fn provider(&self) -> InsertionProvider {
            InsertionProvider::Wtype
        }

        fn insert_text(&mut self, text: &str) -> Result<(), InsertionError> {
            if self.fail {
                return Err(InsertionError {
                    kind: InsertionErrorKind::Rejected,
                    message: "provider refused".to_string(),
                });
            }
            self.inserted
                .lock()
                .expect("fake backend")
                .push(text.to_string());
            Ok(())
        }
    }

    fn contexts() -> RenderContext<'static> {
        RenderContext {
            local_time: Some(TIME),
            timezone: Some("CEST"),
            clipboard: Some("pasted value"),
            clipboard_available: true,
        }
    }

    #[test]
    fn rendering_substitutes_local_time_and_clips_the_clipboard_value() {
        let policy = SnippetPolicy {
            clipboard_variable_bytes: 8,
            ..SnippetPolicy::default()
        };
        let snippet = Snippet::new(
            "Stamp",
            None,
            None,
            "{{date}} {{time}} {{timezone}} {{utc_offset}} | {{clipboard}}",
        );

        let rendered = render(&snippet, policy, &contexts());

        assert_eq!(rendered.text, "2026-10-02 09:05:07 CEST +02:00 | pasted v");
        assert!(rendered.clipboard_truncated);
        assert!(!rendered.clipboard_empty);
        assert_eq!(
            rendered.variables,
            vec![
                SnippetVariable::Date,
                SnippetVariable::Time,
                SnippetVariable::Timezone,
                SnippetVariable::UtcOffset,
                SnippetVariable::Clipboard
            ]
        );
    }

    #[test]
    fn preview_rendering_is_deterministic_and_reads_nothing() {
        let snippet = Snippet::new("Stamp", None, None, "{{date}} {{clipboard}}");

        let first = render(
            &snippet,
            SnippetPolicy::default(),
            &RenderContext::preview(),
        );
        let second = render(
            &snippet,
            SnippetPolicy::default(),
            &RenderContext::preview(),
        );

        assert_eq!(first, second, "previews do not depend on the current time");
        assert_eq!(first.text, "«date» «clipboard»");
        assert!(!first.clipboard_truncated);
        assert!(!first.clipboard_empty);
    }

    #[test]
    fn an_empty_selection_renders_a_marker_instead_of_content() {
        let snippet = Snippet::new("Paste", None, None, "before {{clipboard}} after");
        let rendered = render(
            &snippet,
            SnippetPolicy::default(),
            &RenderContext {
                clipboard: None,
                clipboard_available: true,
                ..RenderContext::preview()
            },
        );

        assert_eq!(rendered.text, format!("before {CLIPBOARD_EMPTY} after"));
        assert!(rendered.clipboard_empty);
        assert!(!rendered.text.contains(CLIPBOARD_PLACEHOLDER));
    }

    #[test]
    fn clip_text_respects_character_boundaries() {
        assert_eq!(clip_text("abc", 8), ("abc".to_string(), false));
        let (clipped, truncated) = clip_text("üüüü", 3);
        assert_eq!(clipped, "ü", "clipping never splits a character");
        assert!(truncated);
        assert_eq!(clip_text("", 4), (String::new(), false));
    }

    #[test]
    fn library_enforces_unique_names_and_conflict_aware_triggers() {
        let policy = SnippetPolicy::default();
        let mut library = SnippetLibrary::default();
        library
            .upsert(
                Snippet::new("Address", None, Some(";addr".to_string()), "Street 1"),
                policy,
            )
            .expect("the first snippet is accepted");
        library
            .upsert(
                Snippet::new("Reply", Some("Mail".to_string()), None, "Thanks"),
                policy,
            )
            .expect("a snippet without a trigger is accepted");

        assert_eq!(
            library.upsert(
                Snippet::new("Other", None, Some(";ADDR".to_string()), "x"),
                policy
            ),
            Err(SnippetError::DuplicateTrigger {
                trigger: ";ADDR".to_string()
            })
        );

        assert_eq!(
            library.upsert(
                Snippet::new("Bad", None, Some("addr".to_string()), "x"),
                policy
            ),
            Err(SnippetError::TriggerNotDelimited {
                trigger: "addr".to_string()
            })
        );

        assert_eq!(
            library.upsert(Snippet::new("address", None, None, "x"), policy),
            Err(SnippetError::DuplicateName {
                name: "address".to_string()
            })
        );

        library
            .upsert(
                Snippet::new("Address", None, Some(";addr".to_string()), "Street 2"),
                policy,
            )
            .expect("replacing a snippet keeps its own trigger");
        assert_eq!(
            library
                .get("Address")
                .map(|snippet| snippet.content.as_str()),
            Some("Street 2")
        );
        assert_eq!(library.names(), vec!["Address", "Reply"]);
        assert_eq!(library.folders(), vec!["Mail"]);
        assert_eq!(
            library.remove("Missing"),
            Err(SnippetError::UnknownSnippet {
                name: "Missing".to_string()
            })
        );
        assert!(library.remove("Reply").is_ok());
        assert_eq!(library.len(), 1);
    }

    #[test]
    fn library_search_matches_names_folders_triggers_and_content() {
        let policy = SnippetPolicy::default();
        let mut library = SnippetLibrary::default();
        library
            .upsert(
                Snippet::new(
                    "Address",
                    Some("Contact".to_string()),
                    Some(";addr".to_string()),
                    "Street 1",
                ),
                policy,
            )
            .unwrap();
        library
            .upsert(
                Snippet::new("Signature", None, None, "Best regards"),
                policy,
            )
            .unwrap();

        assert_eq!(library.search("addr", 10, policy).len(), 1);
        assert_eq!(library.search("CONTACT", 10, policy).len(), 1);
        assert_eq!(library.search("regards", 10, policy).len(), 1);
        assert_eq!(
            library.search("", 10, policy).len(),
            2,
            "an empty query lists the library"
        );
        assert_eq!(library.search("", 1, policy).len(), 1);
        assert!(library.search("nothing here", 10, policy).is_empty());

        let preview = &library.search("signature", 10, policy)[0].preview;
        assert_eq!(preview, "Best regards");
    }

    #[test]
    fn library_validation_reports_the_first_conflict() {
        let policy = SnippetPolicy::default();
        let library = SnippetLibrary::from_snippets(vec![
            Snippet::new("One", None, Some(";dup".to_string()), "a"),
            Snippet::new("Two", None, Some(";dup".to_string()), "b"),
        ]);

        assert_eq!(
            library.validate(policy),
            Err(SnippetError::DuplicateTrigger {
                trigger: ";dup".to_string()
            })
        );
    }

    #[test]
    fn insertion_is_disabled_without_a_verified_provider() {
        let snippet = Snippet::new("Test", None, None, "hello {{clipboard}}");
        let mut service = SnippetInsertionService::<FakeBackend>::new(Err(InsertionError {
            kind: InsertionErrorKind::MissingDependency,
            message: "no verified insertion provider was found".to_string(),
        }));

        assert!(!service.available());
        assert!(service.provider().is_none());
        assert_eq!(
            service.unavailable_reason(),
            Some("no verified insertion provider was found")
        );
        let error = service
            .insert(&snippet, SnippetPolicy::default(), &contexts())
            .expect_err("expansion stays disabled");
        assert!(matches!(
            error,
            SnippetServiceError::ExpansionUnavailable { .. }
        ));
        assert!(error.to_string().contains("disabled"));
    }

    #[test]
    fn insertion_renders_once_and_reports_the_outcome() {
        let inserted = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend {
            inserted: Arc::clone(&inserted),
            fail: false,
        };
        let mut service = SnippetInsertionService::new(Ok(backend));
        assert!(service.available());
        assert_eq!(service.provider(), Some(InsertionProvider::Wtype));

        let snippet = Snippet::new("Stamp", None, None, "{{date}} {{clipboard}}");
        let report = service
            .insert(&snippet, SnippetPolicy::default(), &contexts())
            .expect("insertion succeeds");

        assert_eq!(report.provider, InsertionProvider::Wtype);
        assert_eq!(report.bytes, "2026-10-02 pasted value".len());
        assert!(!report.clipboard_truncated);
        assert_eq!(
            inserted.lock().expect("fake backend").as_slice(),
            &["2026-10-02 pasted value".to_string()]
        );
    }

    #[test]
    fn provider_failures_are_reported_unchanged() {
        let mut service = SnippetInsertionService::new(Ok(FakeBackend {
            inserted: Arc::new(Mutex::new(Vec::new())),
            fail: true,
        }));

        let error = service
            .insert(
                &Snippet::new("Test", None, None, "text"),
                SnippetPolicy::default(),
                &RenderContext::preview(),
            )
            .expect_err("a refusing provider fails the insert");

        assert_eq!(
            error,
            SnippetServiceError::Insertion(InsertionError {
                kind: InsertionErrorKind::Rejected,
                message: "provider refused".to_string()
            })
        );
    }

    #[test]
    fn policy_projects_the_configuration() {
        let policy = SnippetPolicy::from_configuration(&kestrel_core::SnippetConfiguration {
            max_content_bytes: 2048,
            clipboard_variable_bytes: 128,
            insert_timeout_millis: 750,
            preferred_provider: SnippetProviderPreference::Xdotool,
            expansion_timing: kestrel_core::SnippetExpansionTiming::Delimiter,
        });

        assert_eq!(policy.max_content_bytes, 2048);
        assert_eq!(policy.clipboard_variable_bytes, 128);
        assert_eq!(policy.insert_timeout, std::time::Duration::from_millis(750));
        assert_eq!(
            SnippetPolicy::default().max_content_bytes,
            kestrel_core::DEFAULT_SNIPPET_CONTENT_BYTES
        );
    }
}
