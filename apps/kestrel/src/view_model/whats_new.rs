//! This release's notes for the Overview banner and the notes dialog.

use kestrel_core::ApplicationConfiguration;

use super::FeatureViewModel;
use crate::whats_new::{APP_WIDE, CURRENT_VERSION, Change, in_use};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WhatsNewViewModel {
    pub version: String,
    /// The setting that lets Overview offer the notes after an update.
    pub offered: bool,
    /// Shown on Overview once after an update that changes a feature in use.
    pub banner: Option<String>,
    /// Features in use first (app-wide notes lead), then the rest.
    pub sections: Vec<WhatsNewSection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhatsNewSection {
    pub title: String,
    /// The feature is on, or the notes concern every user.
    pub in_use: bool,
    pub changes: Vec<String>,
}

impl WhatsNewViewModel {
    pub(crate) fn new(
        changes: &[Change],
        pending: bool,
        features: &[FeatureViewModel],
        configuration: &ApplicationConfiguration,
    ) -> Self {
        let section = |feature: &str, title: &str| WhatsNewSection {
            title: title.to_owned(),
            in_use: in_use(configuration, feature),
            changes: changes
                .iter()
                .filter(|change| change.feature == feature)
                .map(|change| change.text.clone())
                .collect(),
        };
        let mut sections = std::iter::once(section(APP_WIDE, "Kestrel"))
            .chain(
                features
                    .iter()
                    .map(|feature| section(&feature.id, &feature.label)),
            )
            .filter(|section| !section.changes.is_empty())
            .collect::<Vec<_>>();
        // A stable sort keeps app-wide notes first and features in hub order.
        sections.sort_by_key(|section| !section.in_use);
        let changes_in_use = sections
            .iter()
            .filter(|section| section.in_use)
            .map(|section| section.changes.len())
            .sum::<usize>();
        Self {
            version: CURRENT_VERSION.to_owned(),
            offered: configuration.startup.whats_new,
            banner: (pending && configuration.startup.whats_new && changes_in_use > 0).then(|| {
                format!(
                    "Kestrel {CURRENT_VERSION}: {changes_in_use} {} to features you use",
                    if changes_in_use == 1 {
                        "change"
                    } else {
                        "changes"
                    }
                )
            }),
            sections,
        }
    }
}
