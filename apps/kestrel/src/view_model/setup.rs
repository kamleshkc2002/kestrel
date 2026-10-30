//! First-launch setup presentation: the preset choices and their
//! explanation rows.

use crate::{
    FeaturePreset,
    setup::{SensitiveFeature, preset_detail, sensitive_rows},
};

use super::FeatureViewModel;

/// One feature explained before a preset is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupSensitiveViewModel {
    pub feature_id: &'static str,
    pub label: String,
    pub uses: &'static str,
    pub keeps: &'static str,
}

/// One choice on the preset step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupPresetViewModel {
    /// `None` for the choice that changes nothing.
    pub preset: Option<FeaturePreset>,
    pub title: &'static str,
    /// The features it turns on, by label.
    pub detail: String,
    /// The chosen features that explain their use before they are enabled.
    pub sensitive: Vec<SetupSensitiveViewModel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupViewModel {
    pub presets: Vec<SetupPresetViewModel>,
}

impl SetupViewModel {
    pub fn new(features: &[FeatureViewModel]) -> Self {
        let registered = features
            .iter()
            .filter(|feature| feature.configurable && feature.id != crate::STATUS_NOTIFIER_ID)
            .collect::<Vec<_>>();
        let ids = registered
            .iter()
            .map(|feature| feature.id.as_str())
            .collect::<Vec<_>>();
        let labelled = registered
            .iter()
            .map(|feature| (feature.id.as_str(), feature.label.as_str()))
            .collect::<Vec<_>>();
        let mut presets = [
            FeaturePreset::Essentials,
            FeaturePreset::Balanced,
            FeaturePreset::Everything,
        ]
        .into_iter()
        .map(|preset| SetupPresetViewModel {
            preset: Some(preset),
            title: preset.label(),
            detail: preset_detail(preset, &labelled),
            sensitive: sensitive_view_rows(
                &registered,
                sensitive_rows(preset, ids.iter().copied()),
            ),
        })
        .collect::<Vec<_>>();
        presets.push(SetupPresetViewModel {
            preset: None,
            title: "I'll choose myself",
            detail: "Nothing changes; pick features in the Feature Hub.".to_owned(),
            sensitive: Vec::new(),
        });
        Self { presets }
    }
}

fn sensitive_view_rows(
    registered: &[&FeatureViewModel],
    rows: Vec<&'static SensitiveFeature>,
) -> Vec<SetupSensitiveViewModel> {
    rows.into_iter()
        .filter_map(|row| {
            registered
                .iter()
                .find(|feature| feature.id == row.feature_id)
                .map(|feature| SetupSensitiveViewModel {
                    feature_id: row.feature_id,
                    label: feature.label.clone(),
                    uses: row.uses,
                    keeps: row.keeps,
                })
        })
        .collect()
}
