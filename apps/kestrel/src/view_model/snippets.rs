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

#[derive(Debug, Clone, Copy, Default)]
pub struct SnippetQuery<'a> {
    pub query: &'a str,
    pub matches: &'a [SnippetMatch],
    pub draft: Option<&'a SnippetDraft>,
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
    pub expansion_timing: SnippetExpansionTiming,
    /// Configured preference, which may differ from discovery.
    pub provider_preference: SnippetProviderPreference,
    pub search_query: &'a str,
    pub matches: &'a [SnippetMatch],
    pub draft: Option<&'a SnippetDraft>,
    pub warnings: &'a [ConfigurationWarning],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetsViewModel {
    pub running: bool,
    pub status: String,
    pub insertion_status: String,
    pub insertion_available: bool,
    pub directory: Option<String>,
    pub expansion_label: &'static str,
    pub search_query: String,
    pub items: Vec<SnippetItemViewModel>,
    pub total: usize,
    pub draft: Option<SnippetDraftViewModel>,
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
    pub expansion_timing: SnippetExpansionTiming,
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
            expansion_timing: presentation.expansion_timing,
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
            expansion_label: presentation.expansion_timing.label(),
            search_query: presentation.search_query.to_owned(),
            items,
            total,
            draft: presentation.draft.map(|draft| SnippetDraftViewModel {
                name: draft.name.clone(),
                folder: draft.folder.clone(),
                trigger: draft.trigger.clone(),
                content: draft.content.clone(),
                is_new: draft.name.trim().is_empty(),
            }),
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
