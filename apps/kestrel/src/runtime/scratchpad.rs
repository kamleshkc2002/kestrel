//! Scratchpad runtime features.

use super::*;

impl ApplicationRuntime {
    pub fn scratchpad_view_model(&self) -> ScratchpadViewModel {
        ScratchpadViewModel::from_presentation(ScratchpadPresentation {
            running: self.scratchpad_is_running(),
            warning: self.scratchpad_warning.clone(),
            snapshot: self.scratchpad_snapshot(SystemTime::now()),
            limits: self.scratchpad_policy,
        })
    }

    pub fn scratchpad_is_running(&self) -> bool {
        self.registry
            .registrations()
            .any(|registration| registration.feature.id == SCRATCHPAD_ID && registration.running)
    }

    pub fn scratchpad_snapshot(&self, now: SystemTime) -> Option<ScratchpadSnapshot> {
        self.scratchpad
            .as_ref()
            .map(|service| service.snapshot(now))
    }

    pub fn scratchpad_service(&mut self) -> Result<&mut ScratchpadService, String> {
        if !self.scratchpad_is_running() {
            return Err(
                "Scratchpad is not running; enable notes.scratchpad in the Feature Hub.".to_owned(),
            );
        }
        self.scratchpad.as_mut().ok_or_else(|| {
            "No writable scratchpad data directory was resolved; set XDG_DATA_HOME or HOME."
                .to_owned()
        })
    }

    pub fn scratchpad_create(&mut self, title: String) -> Result<u64, String> {
        self.scratchpad_service()?
            .create(title, SystemTime::now())
            .map(|id| id.get())
            .map_err(|error| error.to_string())
    }
    pub fn scratchpad_rename(&mut self, id: u64, title: String) -> Result<(), String> {
        self.scratchpad_service()?
            .rename(
                kestrel_services::scratchpad::NoteId::from(id),
                title,
                SystemTime::now(),
            )
            .map_err(|error| error.to_string())
    }
    pub fn scratchpad_switch(&mut self, id: u64) -> Result<(), String> {
        self.scratchpad_service()?
            .switch(kestrel_services::scratchpad::NoteId::from(id))
            .map_err(|error| error.to_string())
    }
    pub fn scratchpad_close(&mut self, id: u64) -> Result<(), String> {
        self.scratchpad_service()?
            .close(kestrel_services::scratchpad::NoteId::from(id))
            .map_err(|error| error.to_string())
    }
    pub fn scratchpad_update(&mut self, id: u64, text: String) -> Result<(), String> {
        self.scratchpad_service()?
            .update_text(
                kestrel_services::scratchpad::NoteId::from(id),
                text,
                SystemTime::now(),
            )
            .map_err(|error| error.to_string())
    }
    pub fn scratchpad_remove_expired(&mut self) -> Result<usize, String> {
        self.scratchpad_service()?
            .remove_expired(SystemTime::now())
            .map_err(|error| error.to_string())
    }
    pub fn scratchpad_export(&mut self, id: u64, destination: &Path) -> Result<(), String> {
        self.scratchpad_service()?
            .export(kestrel_services::scratchpad::NoteId::from(id), destination)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_scratchpad_refuses_commands() {
        let mut runtime =
            ApplicationRuntime::new(&ApplicationConfiguration::default()).expect("runtime builds");
        runtime.start();
        let error = runtime
            .scratchpad_create("test".to_owned())
            .expect_err("disabled feature");
        assert!(error.contains("notes.scratchpad"));
    }
}
