//! Text snippet presentation.

use super::*;

/// A snippet draft before validation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnippetDraft {
    pub name: String,
    pub folder: String,
    pub trigger: String,
    pub content: String,
}

/// Editor field a rejected save points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnippetDraftField {
    Name,
    Folder,
    Trigger,
    Content,
}

/// A rejected save shown beside the field that caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetDraftError {
    pub field: SnippetDraftField,
    pub message: String,
}

impl SnippetDraftError {
    pub fn from_error(error: &kestrel_core::SnippetError) -> Self {
        use kestrel_core::SnippetError;
        let field = match error {
            SnippetError::EmptyName
            | SnippetError::NameTooLong { .. }
            | SnippetError::DuplicateName { .. } => SnippetDraftField::Name,
            SnippetError::EmptyFolder | SnippetError::FolderTooLong { .. } => {
                SnippetDraftField::Folder
            }
            SnippetError::EmptyTrigger
            | SnippetError::TriggerTooLong { .. }
            | SnippetError::TriggerContainsWhitespace { .. }
            | SnippetError::DuplicateTrigger { .. }
            | SnippetError::TriggerNotDelimited { .. } => SnippetDraftField::Trigger,
            SnippetError::ContentTooLong { .. }
            | SnippetError::TooManyVariables { .. }
            | SnippetError::UnknownVariable { .. }
            | SnippetError::UnknownSnippet { .. } => SnippetDraftField::Content,
        };
        Self {
            field,
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SnippetQuery<'a> {
    pub query: &'a str,
    pub matches: &'a [SnippetMatch],
    pub draft: Option<&'a SnippetDraft>,
    pub error: Option<&'a SnippetDraftError>,
}

pub(crate) const SNIPPETS_OFF: &str = "Snippets are off. Enable snippets.text in the Feature Hub.";

pub(crate) struct SnippetsPresentation<'a> {
    pub library: &'a SnippetLibrary,
    pub policy: SnippetPolicy,
    pub running: bool,
    /// Verified insertion provider, if any.
    pub provider: Option<InsertionProvider>,
    pub unavailable_reason: Option<&'a str>,
    /// Why the feature is not running, when it is not.
    pub stopped_reason: Option<String>,
    pub directory: Option<String>,
    /// Configured preference, which may differ from discovery.
    pub provider_preference: SnippetProviderPreference,
    pub search_query: &'a str,
    pub matches: &'a [SnippetMatch],
    pub draft: Option<&'a SnippetDraft>,
    pub draft_error: Option<&'a SnippetDraftError>,
    pub warnings: &'a [ConfigurationWarning],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetsViewModel {
    pub running: bool,
    pub status: String,
    pub insertion_status: String,
    pub insertion_available: bool,
    pub directory: Option<String>,
    pub search_query: String,
    pub items: Vec<SnippetItemViewModel>,
    pub total: usize,
    pub draft: Option<SnippetDraftViewModel>,
    pub draft_error: Option<SnippetDraftError>,
    pub warnings: Vec<ConfigurationWarningViewModel>,
    pub policy: SnippetPolicyViewModel,
    pub turn_on: Option<TurnOnViewModel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetItemViewModel {
    pub name: String,
    pub folder: Option<String>,
    pub trigger: Option<String>,
    pub detail: String,
    pub preview: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnippetDraftViewModel {
    pub name: String,
    pub folder: String,
    pub trigger: String,
    pub content: String,
    pub is_new: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnippetPolicyViewModel {
    pub max_content_bytes: u32,
    pub clipboard_variable_bytes: u32,
    pub insert_timeout_millis: u64,
    pub preferred_provider: SnippetProviderPreference,
    pub bounds: SnippetBoundsViewModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnippetBoundsViewModel {
    pub min_content_bytes: u32,
    pub max_content_bytes: u32,
    pub min_clipboard_bytes: u32,
    pub max_clipboard_bytes: u32,
    pub min_insert_timeout_millis: u64,
    pub max_insert_timeout_millis: u64,
}

impl SnippetsViewModel {
    pub(crate) fn from_presentation(presentation: SnippetsPresentation<'_>) -> Self {
        let policy = SnippetPolicyViewModel {
            max_content_bytes: presentation.policy.max_content_bytes,
            clipboard_variable_bytes: presentation.policy.clipboard_variable_bytes,
            insert_timeout_millis: presentation.policy.insert_timeout.as_millis() as u64,
            preferred_provider: presentation.provider_preference,
            bounds: SnippetBoundsViewModel {
                min_content_bytes: kestrel_core::MIN_SNIPPET_CONTENT_BYTES,
                max_content_bytes: kestrel_core::MAX_SNIPPET_CONTENT_BYTES,
                min_clipboard_bytes: kestrel_core::MIN_SNIPPET_CLIPBOARD_BYTES,
                max_clipboard_bytes: kestrel_core::MAX_SNIPPET_CLIPBOARD_BYTES,
                min_insert_timeout_millis: kestrel_core::MIN_SNIPPET_INSERT_TIMEOUT_MILLIS,
                max_insert_timeout_millis: kestrel_core::MAX_SNIPPET_INSERT_TIMEOUT_MILLIS,
            },
        };
        let provider = presentation.provider;
        let insertion_available = provider.is_some();
        let insertion_status = match provider {
            Some(provider) => format!(
                "Insertion uses {} ({})",
                provider.label(),
                provider.transport()
            ),
            None => presentation
                .unavailable_reason
                .map(str::to_owned)
                .unwrap_or_else(|| "no verified insertion provider".to_owned()),
        };
        let items = presentation
            .matches
            .iter()
            .map(|matched| SnippetItemViewModel {
                name: matched.name.clone(),
                folder: matched.folder.clone(),
                trigger: matched.trigger.clone(),
                detail: snippet_detail(matched),
                preview: matched.preview.clone(),
            })
            .collect::<Vec<_>>();
        let total = presentation.library.len();

        Self {
            running: presentation.running,
            status: if presentation.running {
                let mut status = format!("{} of {total} snippets shown", items.len());
                if !presentation.library.folders().is_empty() {
                    status.push_str(&format!(
                        " · {} folders",
                        presentation.library.folders().len()
                    ));
                }
                status.push_str(&format!(
                    " · content bound {}",
                    format_bytes(presentation.policy.max_content_bytes as f64)
                ));
                status
            } else {
                presentation
                    .stopped_reason
                    .unwrap_or_else(|| SNIPPETS_OFF.to_owned())
            },
            insertion_status,
            insertion_available,
            directory: presentation.directory,
            search_query: presentation.search_query.to_owned(),
            items,
            total,
            draft: presentation.draft.map(|draft| SnippetDraftViewModel {
                name: draft.name.clone(),
                folder: draft.folder.clone(),
                trigger: draft.trigger.clone(),
                content: draft.content.clone(),
                is_new: presentation.library.get(draft.name.trim()).is_none(),
            }),
            draft_error: presentation.draft_error.cloned(),
            warnings: presentation
                .warnings
                .iter()
                .map(ConfigurationWarningViewModel::from)
                .collect(),
            policy,
            turn_on: None,
        }
    }
}

fn snippet_detail(matched: &SnippetMatch) -> String {
    let mut parts = Vec::new();
    if let Some(folder) = &matched.folder {
        parts.push(folder.clone());
    }
    if let Some(trigger) = &matched.trigger {
        parts.push(format!("trigger {trigger}"));
    }
    if matched.variables.is_empty() {
        parts.push("no variables".to_owned());
    } else {
        parts.push(
            matched
                .variables
                .iter()
                .map(|variable| variable.token())
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view_model::test_support::*;

    #[test]
    fn snippets_presentation_names_the_provider_and_previews_deterministically() {
        let library = snippet_library();
        let matches = library.search("", 10, SnippetPolicy::default());
        let snippets =
            SnippetsViewModel::from_presentation(snippet_presentation(&library, &matches));

        assert!(snippets.insertion_available);
        assert!(snippets.insertion_status.contains("wtype"));
        let item = snippets
            .items
            .first()
            .expect("the searched snippet is shown");
        assert!(
            item.preview.contains("«date»") && item.preview.contains("«clipboard»"),
            "previews keep variables as placeholders instead of reading live values: {}",
            item.preview
        );
        assert!(item.detail.contains("{{date}}"));
    }

    #[test]
    fn a_rejected_save_keeps_the_draft_and_targets_its_field() {
        let library = snippet_library();
        let matches = library.search("", 10, SnippetPolicy::default());
        let draft = SnippetDraft {
            name: "Home".to_owned(),
            folder: String::new(),
            trigger: "addr".to_owned(),
            content: "Home address".to_owned(),
        };
        for (error, field, message) in [
            (
                kestrel_core::SnippetError::TriggerNotDelimited {
                    trigger: "addr".to_owned(),
                },
                SnippetDraftField::Trigger,
                "A trigger must start with a symbol, such as ;addr.",
            ),
            (
                kestrel_core::SnippetError::DuplicateTrigger {
                    trigger: ";addr".to_owned(),
                },
                SnippetDraftField::Trigger,
                "Another snippet already uses the trigger ;addr.",
            ),
            (
                kestrel_core::SnippetError::DuplicateName {
                    name: "Home".to_owned(),
                },
                SnippetDraftField::Name,
                "Another snippet is already called Home.",
            ),
        ] {
            let error = SnippetDraftError::from_error(&error);
            let mut presentation = snippet_presentation(&library, &matches);
            presentation.draft = Some(&draft);
            presentation.draft_error = Some(&error);

            let snippets = SnippetsViewModel::from_presentation(presentation);
            let kept = snippets.draft.as_ref().expect("the draft is kept");
            assert_eq!(kept.name, "Home");
            assert_eq!(kept.folder, "");
            assert_eq!(kept.trigger, "addr");
            assert_eq!(kept.content, "Home address");
            assert!(kept.is_new, "a rejected new snippet is still new");
            let shown = snippets.draft_error.as_ref().expect("the error is shown");
            assert_eq!(shown.field, field);
            assert_eq!(shown.message, message);
        }

        let stored = SnippetDraft {
            name: "Address".to_owned(),
            ..draft
        };
        let mut presentation = snippet_presentation(&library, &matches);
        presentation.draft = Some(&stored);
        let snippets = SnippetsViewModel::from_presentation(presentation);
        assert!(!snippets.draft.expect("the draft is shown").is_new);
    }

    #[test]
    fn snippets_presentation_disables_insertion_without_a_provider() {
        let library = snippet_library();
        let matches = library.search("", 10, SnippetPolicy::default());
        let mut presentation = snippet_presentation(&library, &matches);
        presentation.provider = None;
        presentation.unavailable_reason = Some("no verified insertion provider was found");

        let snippets = SnippetsViewModel::from_presentation(presentation);

        assert!(!snippets.insertion_available);
        assert_eq!(
            snippets.insertion_status,
            "no verified insertion provider was found"
        );
    }
}
