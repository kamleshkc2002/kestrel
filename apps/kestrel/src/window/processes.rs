//! Process inspector and listening-port groups.

use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation, accessible::Property};
use kestrel::{
    ApplicationCommand, ListenersViewModel, ProcessActionKind, ProcessRequest, ProcessRowViewModel,
    ProcessesViewModel,
};

use super::widgets::{SearchGuard, plain_row};

/// Widgets kept across updates so the search keeps its text and focus.
pub(super) struct ProcessesPanel {
    pub(super) container: gtk::Box,
    pub(super) search: gtk::SearchEntry,
    process_group: adw::PreferencesGroup,
    process_rows: gtk::ListBox,
    stale: gtk::Label,
    listener_group: adw::PreferencesGroup,
    listener_rows: gtk::ListBox,
    /// Open buttons appear only when the desktop has an http handler.
    browser: bool,
}

impl ProcessesPanel {
    pub(super) fn set_processes(
        &self,
        processes: &ProcessesViewModel,
        commands: &Sender<ApplicationCommand>,
    ) {
        self.process_group
            .set_description(Some(&escape(&processes.status)));
        self.stale
            .set_text(processes.stale_notice.as_deref().unwrap_or(""));
        self.stale.set_visible(processes.stale_notice.is_some());
        clear(&self.process_rows);
        if processes.rows.is_empty() && processes.running {
            self.process_rows.append(
                &plain_row()
                    .title(if processes.query.is_empty() {
                        "No processes"
                    } else {
                        "No matching processes"
                    })
                    .build(),
            );
        }
        for row in &processes.rows {
            self.process_rows.append(&process_row(row, commands));
        }
        self.process_rows.set_visible(processes.running);
    }

    pub(super) fn set_listeners(
        &self,
        listeners: &ListenersViewModel,
        commands: &Sender<ApplicationCommand>,
    ) {
        self.listener_group
            .set_description(Some(&escape(&listeners.status)));
        clear(&self.listener_rows);
        for row in &listeners.rows {
            let item = plain_row().title(&row.title).subtitle(&row.owner).build();
            item.add_suffix(&copy_button("Port", row.port.to_string(), commands));
            item.add_suffix(&copy_button("Address", row.address.clone(), commands));
            if let Some(pid) = row.pid {
                item.add_suffix(&copy_button("PID", pid.to_string(), commands));
            }
            if let Some(url) = row.open_url.as_ref().filter(|_| self.browser) {
                let open = flat_button("web-browser-symbolic", &format!("Open {url}"));
                let sender = commands.clone();
                let url = url.clone();
                open.connect_clicked(move |_| {
                    let _ = sender.try_send(ApplicationCommand::OpenUri(url.clone()));
                });
                item.add_suffix(&open);
            }
            if listeners.can_stop {
                let stop = gtk::Button::with_label("Stop owner");
                stop.set_valign(Align::Center);
                stop.add_css_class("flat");
                match (row.owner_identity, &row.stop_disabled_reason) {
                    (Some(identity), None) => {
                        let sender = commands.clone();
                        stop.connect_clicked(move |_| {
                            let _ = sender.try_send(ApplicationCommand::Processes(
                                ProcessRequest::Confirm {
                                    identity,
                                    action: ProcessActionKind::Terminate,
                                },
                            ));
                        });
                    }
                    (_, reason) => {
                        stop.set_sensitive(false);
                        item.set_tooltip_text(reason.as_deref());
                    }
                }
                item.add_suffix(&stop);
            }
            self.listener_rows.append(&item);
        }
        self.listener_rows
            .set_visible(listeners.running && !listeners.rows.is_empty());
    }
}

pub(super) fn build_processes_panel(
    processes: &ProcessesViewModel,
    listeners: &ListenersViewModel,
    commands: &Sender<ApplicationCommand>,
) -> ProcessesPanel {
    let container = gtk::Box::new(Orientation::Vertical, 12);

    let process_group = adw::PreferencesGroup::builder().title("Processes").build();
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Name, executable, owner, or PID")
        .hexpand(true)
        .build();
    search.update_property(&[Property::Label("Search processes")]);
    let guard: SearchGuard = std::rc::Rc::new(std::cell::Cell::new(true));
    search.set_text(&processes.query);
    guard.set(false);
    let sender = commands.clone();
    let handler_guard = std::rc::Rc::clone(&guard);
    search.connect_search_changed(move |entry| {
        if handler_guard.get() {
            return;
        }
        let _ = sender.try_send(ApplicationCommand::Processes(ProcessRequest::Search(
            entry.text().to_string(),
        )));
    });
    let refresh = flat_button("view-refresh-symbolic", "Refresh processes");
    let sender = commands.clone();
    refresh.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::Processes(ProcessRequest::Refresh));
    });
    let search_row = adw::ActionRow::new();
    search_row.set_title("Search");
    search_row.add_suffix(&search);
    search_row.add_suffix(&refresh);
    search_row.set_activatable_widget(Some(&search));
    process_group.add(&search_row);
    let stale = gtk::Label::new(None);
    stale.add_css_class("warning");
    stale.set_wrap(true);
    stale.set_xalign(0.0);
    let process_rows = boxed_list();
    process_group.add(&stale);
    process_group.add(&process_rows);
    search_row.set_visible(processes.running);
    container.append(&process_group);

    let listener_group = adw::PreferencesGroup::builder()
        .title("Listening ports")
        .build();
    let listener_rows = boxed_list();
    listener_group.add(&listener_rows);
    container.append(&listener_group);

    let panel = ProcessesPanel {
        container,
        search,
        process_group,
        process_rows,
        stale,
        listener_group,
        listener_rows,
        browser: gtk::gio::AppInfo::default_for_uri_scheme("http").is_some(),
    };
    panel.set_processes(processes, commands);
    panel.set_listeners(listeners, commands);
    panel
}

fn process_row(row: &ProcessRowViewModel, commands: &Sender<ApplicationCommand>) -> adw::ActionRow {
    let subtitle = match &row.disabled_reason {
        Some(reason) => format!("{}\n{reason}", row.subtitle),
        None => row.subtitle.clone(),
    };
    let item = plain_row()
        .title(&row.title)
        .subtitle(subtitle)
        .subtitle_lines(0)
        .build();
    for action in &row.actions {
        let label = match action.action {
            ProcessActionKind::Terminate => "End",
            ProcessActionKind::Kill => "Kill",
            ProcessActionKind::Restart { .. } => "Restart",
        };
        let button = gtk::Button::with_label(label);
        button.set_valign(Align::Center);
        button.add_css_class("flat");
        if action.action == ProcessActionKind::Kill {
            button.add_css_class("error");
        }
        button.set_tooltip_text(Some(&action.label));
        let sender = commands.clone();
        let identity = row.identity;
        let kind = action.action;
        button.connect_clicked(move |_| {
            let _ = sender.try_send(ApplicationCommand::Processes(ProcessRequest::Confirm {
                identity,
                action: kind,
            }));
        });
        item.add_suffix(&button);
    }
    item
}

fn copy_button(what: &str, value: String, commands: &Sender<ApplicationCommand>) -> gtk::Button {
    let button = gtk::Button::with_label(what);
    button.set_valign(Align::Center);
    button.add_css_class("flat");
    button.set_tooltip_text(Some(&format!("Copy {}", what.to_lowercase())));
    let sender = commands.clone();
    button.connect_clicked(move |_| {
        let _ = sender.try_send(ApplicationCommand::CopyText(value.clone()));
    });
    button
}

fn flat_button(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_valign(Align::Center);
    button.add_css_class("flat");
    button.set_tooltip_text(Some(label));
    button.update_property(&[Property::Label(label)]);
    button
}

fn boxed_list() -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    list
}

fn clear(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn escape(text: &str) -> String {
    gtk::glib::markup_escape_text(text).to_string()
}
