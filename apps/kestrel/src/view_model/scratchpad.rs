//! Scratchpad presentation and note state.

use super::*;

pub(crate) struct ScratchpadPresentation {
    pub warning: Option<String>,
    pub running: bool,
    pub snapshot: Option<ScratchpadSnapshot>,
    pub limits: kestrel_core::ScratchpadConfiguration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchpadTabViewModel {
    pub id: u64,
    pub title: String,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchpadExpiredViewModel {
    pub id: u64,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchpadViewModel {
    pub running: bool,
    pub status: String,
    pub tabs: Vec<ScratchpadTabViewModel>,
    pub active_id: Option<u64>,
    pub active_title: String,
    pub active_text: String,
    pub preview_markup: String,
    pub expired: Vec<ScratchpadExpiredViewModel>,
    pub warning: Option<String>,
    pub limits: kestrel_core::ScratchpadConfiguration,
}

impl ScratchpadViewModel {
    pub(crate) fn from_presentation(presentation: ScratchpadPresentation) -> Self {
        let presentation_warning = presentation.warning.clone();
        let (tabs, active_id, active_title, active_text, expired, warning) =
            if let Some(snapshot) = presentation.snapshot {
                let tabs = snapshot
                    .notes
                    .iter()
                    .map(|note| ScratchpadTabViewModel {
                        id: note.id.get(),
                        title: note.title.clone(),
                        active: Some(note.id) == snapshot.active,
                    })
                    .collect();
                let active = snapshot
                    .active
                    .and_then(|id| snapshot.notes.iter().find(|note| note.id == id));
                let expired = snapshot
                    .expired
                    .iter()
                    .filter_map(|id| snapshot.notes.iter().find(|note| note.id == *id))
                    .map(|note| ScratchpadExpiredViewModel {
                        id: note.id.get(),
                        title: note.title.clone(),
                    })
                    .collect();
                (
                    tabs,
                    snapshot.active.map(|id| id.get()),
                    active.map(|note| note.title.clone()).unwrap_or_default(),
                    active.map(|note| note.text.clone()).unwrap_or_default(),
                    expired,
                    snapshot.warning.or(presentation_warning.clone()),
                )
            } else {
                (
                    Vec::new(),
                    None,
                    String::new(),
                    String::new(),
                    Vec::new(),
                    presentation_warning,
                )
            };
        let preview_markup = if presentation.running {
            kestrel_services::scratchpad::render_markdown(&active_text)
        } else {
            String::new()
        };
        let status = if !presentation.running {
            "Scratchpad is off; enable notes.scratchpad in the Feature Hub.".to_owned()
        } else if tabs.is_empty() {
            "No notes yet. Create a tab to begin.".to_owned()
        } else {
            format!(
                "{} note{} available.",
                tabs.len(),
                if tabs.len() == 1 { "" } else { "s" }
            )
        };
        Self {
            running: presentation.running,
            status,
            tabs,
            active_id,
            active_title,
            active_text,
            preview_markup,
            expired,
            warning,
            limits: presentation.limits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratchpad_view_model_projects_tabs_preview_and_expiry() {
        let note = kestrel_services::scratchpad::ScratchpadNote {
            id: kestrel_services::scratchpad::NoteId::from(7),
            title: "Idea".to_owned(),
            text: "# Hello & <world>".to_owned(),
            created_at: 1,
            updated_at: 1,
        };
        let model = ScratchpadViewModel::from_presentation(ScratchpadPresentation {
            warning: None,
            running: true,
            snapshot: Some(ScratchpadSnapshot {
                notes: vec![note],
                active: Some(kestrel_services::scratchpad::NoteId::from(7)),
                expired: vec![kestrel_services::scratchpad::NoteId::from(7)],
                warning: Some("save warning".to_owned()),
            }),
            limits: kestrel_core::ScratchpadConfiguration::default(),
        });
        assert_eq!(model.active_id, Some(7));
        assert_eq!(model.tabs[0].title, "Idea");
        assert!(model.preview_markup.contains("&amp;"));
        assert_eq!(model.expired[0].title, "Idea");
        assert_eq!(model.warning.as_deref(), Some("save warning"));
        let off = ScratchpadViewModel::from_presentation(ScratchpadPresentation {
            warning: None,
            running: false,
            snapshot: None,
            limits: kestrel_core::ScratchpadConfiguration::default(),
        });
        assert!(!off.running);
        assert!(off.tabs.is_empty());
    }
}
