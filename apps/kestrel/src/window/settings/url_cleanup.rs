//! Link cleanup preview and local rule editors.

use std::{cell::RefCell, rc::Rc};

use crate::window::widgets::{FilterRows, plain_row, stacked_row};
use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, accessible::Property};
use kestrel::{ApplicationCommand, ApplicationViewModel};

use kestrel_core::{RemovedParam, SiteRule, UrlCleanupErrorKind, UrlRules, clean_url};

struct PreviewWidgets {
    link: gtk::Entry,
    result: gtk::Entry,
    removed: adw::ActionRow,
    error: adw::ActionRow,
    copy: gtk::Button,
}

/// The preview group followed by the rules editor, sharing one rule state.
pub(super) fn url_cleanup_groups(
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut FilterRows,
) -> Vec<adw::PreferencesGroup> {
    let cleanup = &view_model.url_cleanup;
    let sites = Rc::new(RefCell::new(cleanup.rules.sites.clone()));
    let extra = Rc::new(RefCell::new(cleanup.extra_global.clone()));

    let (preview_group, preview) = build_preview_group(cleanup, commands, filter_rows);
    let refresh: Rc<dyn Fn()> = {
        let preview = Rc::clone(&preview);
        let sites = Rc::clone(&sites);
        let extra = Rc::clone(&extra);
        Rc::new(move || {
            let rules = UrlRules::merged(&extra.borrow(), &sites.borrow());
            update_preview(&preview, &rules);
        })
    };
    update_preview(&preview, &cleanup.rules);

    let link_refresh = Rc::clone(&refresh);
    preview.link.connect_changed(move |_| link_refresh());
    let copy_preview = Rc::clone(&preview);
    let sender = commands.clone();
    preview.copy.connect_clicked(move |_| {
        let cleaned = copy_preview.result.text().to_string();
        if !cleaned.is_empty() {
            let _ = sender.try_send(ApplicationCommand::CopyText(cleaned));
        }
    });

    let rules_group = build_rules_group(
        cleanup,
        Rc::clone(&sites),
        Rc::clone(&extra),
        refresh,
        commands,
        filter_rows,
    );
    vec![preview_group, rules_group]
}

fn build_preview_group(
    cleanup: &kestrel::view_model::UrlCleanupViewModel,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut FilterRows,
) -> (adw::PreferencesGroup, Rc<PreviewWidgets>) {
    let group = adw::PreferencesGroup::builder()
        .title("Clean links")
        .description(
            "Tracking parameters are removed from a http or https link here, on this computer. \
             The rest of the link keeps its exact text.",
        )
        .build();

    let link = gtk::Entry::builder()
        .hexpand(true)
        .placeholder_text("https://")
        .build();
    link.update_property(&[Property::Label("Link to clean")]);
    let link_row = stacked_row(
        "Link",
        "Paste or type one link; the result updates as you type.",
        &link,
    );
    link.set_halign(gtk::Align::Fill);
    group.add(&link_row);
    filter_rows.push((
        link_row.clone().upcast(),
        "clean links link preview tracking parameters".to_owned(),
    ));

    let result = gtk::Entry::builder().hexpand(true).editable(false).build();
    result.update_property(&[
        Property::Label("Cleaned link"),
        Property::Description("Read-only result; select it or use Copy cleaned link"),
    ]);
    let result_row = stacked_row(
        "Cleaned link",
        "The link after cleanup. Nothing is copied until you ask.",
        &result,
    );
    result.set_halign(gtk::Align::Fill);
    group.add(&result_row);
    filter_rows.push((
        result_row.clone().upcast(),
        "clean links cleaned result preview".to_owned(),
    ));

    // The removed list and the error show with a result, so the search leaves them alone.
    let removed = plain_row()
        .title("Removed parameters")
        .subtitle("Nothing to remove")
        .subtitle_lines(0)
        .build();
    removed.set_visible(false);
    group.add(&removed);

    let error = plain_row()
        .title("Link error")
        .subtitle("")
        .subtitle_lines(0)
        .build();
    error.set_visible(false);
    group.add(&error);

    let copy = gtk::Button::with_label("Copy cleaned link");
    copy.set_valign(Align::Center);
    copy.set_sensitive(false);
    copy.update_property(&[Property::Label("Copy cleaned link")]);
    let copy_row = plain_row()
        .title("Copy cleaned link")
        .subtitle("Puts the cleaned result on the clipboard.")
        .build();
    copy_row.add_suffix(&copy);
    group.add(&copy_row);
    filter_rows.push((
        copy_row.clone().upcast(),
        "clean links copy cleaned link clipboard".to_owned(),
    ));

    let automatic = plain_row()
        .title("Automatic cleanup")
        .subtitle(
            "Cleans a single copied link as soon as it is copied. Lists and other content are \
             left alone.",
        )
        .subtitle_lines(0)
        .build();
    let automatic_switch = gtk::Switch::builder()
        .active(cleanup.automatic)
        .valign(Align::Center)
        .build();
    automatic_switch.update_property(&[
        Property::Label("Automatic link cleanup"),
        Property::Description(
            "Clean a single copied link automatically; lists and other content are left alone",
        ),
    ]);
    let sender = commands.clone();
    automatic_switch.connect_state_set(move |_, enabled| {
        let _ = sender.try_send(ApplicationCommand::SetUrlCleanupAutomatic(enabled));
        gtk::glib::Propagation::Proceed
    });
    automatic.add_suffix(&automatic_switch);
    automatic.set_activatable_widget(Some(&automatic_switch));
    group.add(&automatic);
    filter_rows.push((
        automatic.clone().upcast(),
        "clean links automatic cleanup copied link preference".to_owned(),
    ));

    (
        group,
        Rc::new(PreviewWidgets {
            link,
            result,
            removed,
            error,
            copy,
        }),
    )
}

fn build_rules_group(
    cleanup: &kestrel::view_model::UrlCleanupViewModel,
    sites: Rc<RefCell<Vec<SiteRule>>>,
    extra: Rc<RefCell<Vec<String>>>,
    refresh: Rc<dyn Fn()>,
    commands: &Sender<ApplicationCommand>,
    filter_rows: &mut FilterRows,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Link cleanup rules")
        .description(
            "Parameter names are comma separated; a trailing * matches a prefix. Turning a site \
             off keeps its parameter names.",
        )
        .build();

    let extra_row = adw::EntryRow::builder()
        .title("Extra parameters for every site")
        .text(cleanup.extra_global.join(", "))
        .show_apply_button(true)
        .build();
    extra_row.update_property(&[
        Property::Label("Extra parameters for every site"),
        Property::Description("Comma-separated parameter names applied to every link"),
    ]);
    let extra_changed = Rc::clone(&extra);
    let refresh_changed = Rc::clone(&refresh);
    extra_row.connect_changed(move |row| {
        *extra_changed.borrow_mut() = parse_params(&row.text());
        refresh_changed();
    });
    let extra_apply = Rc::clone(&extra);
    let sender = commands.clone();
    let refresh_apply = Rc::clone(&refresh);
    extra_row.connect_apply(move |row| {
        let params = parse_params(&row.text());
        *extra_apply.borrow_mut() = params.clone();
        let _ = sender.try_send(ApplicationCommand::SetUrlCleanupExtraGlobal(params));
        refresh_apply();
    });
    group.add(&extra_row);
    filter_rows.push((
        extra_row.clone().upcast(),
        "clean links tracking parameters extra every site".to_owned(),
    ));

    for (index, site) in cleanup.rules.sites.iter().enumerate() {
        let host = site.host_suffix.clone();
        let row = adw::EntryRow::builder()
            .title(host.as_str())
            .text(site.params.join(", "))
            .show_apply_button(true)
            .build();
        let label = format!("{host} tracking parameters");
        row.update_property(&[
            Property::Label(&label),
            Property::Description("Comma-separated parameter names removed for this site"),
        ]);
        let switch = gtk::Switch::builder()
            .active(site.enabled)
            .valign(Align::Center)
            .build();
        switch.update_property(&[Property::Label(&format!("Clean {host} links"))]);
        row.add_suffix(&switch);

        let sites_changed = Rc::clone(&sites);
        let refresh_changed = Rc::clone(&refresh);
        row.connect_changed(move |row| {
            let params = parse_params(&row.text());
            if let Some(site) = sites_changed.borrow_mut().get_mut(index) {
                site.params = params;
            }
            refresh_changed();
        });

        let sites_apply = Rc::clone(&sites);
        let switch_apply = switch.clone();
        let host_apply = host.clone();
        let sender = commands.clone();
        let refresh_apply = Rc::clone(&refresh);
        row.connect_apply(move |row| {
            let params = parse_params(&row.text());
            let enabled = switch_apply.is_active();
            if let Some(site) = sites_apply.borrow_mut().get_mut(index) {
                site.enabled = enabled;
                site.params = params.clone();
            }
            let _ = sender.try_send(ApplicationCommand::SetUrlCleanupSite {
                host: host_apply.clone(),
                enabled,
                params,
            });
            refresh_apply();
        });

        let sites_switch = Rc::clone(&sites);
        let row_switch = row.clone();
        let host_switch = host.clone();
        let sender = commands.clone();
        let refresh_switch = Rc::clone(&refresh);
        switch.connect_state_set(move |_, enabled| {
            let params = parse_params(&row_switch.text());
            if let Some(site) = sites_switch.borrow_mut().get_mut(index) {
                site.enabled = enabled;
                site.params = params.clone();
            }
            let _ = sender.try_send(ApplicationCommand::SetUrlCleanupSite {
                host: host_switch.clone(),
                enabled,
                params,
            });
            refresh_switch();
            gtk::glib::Propagation::Proceed
        });

        group.add(&row);
        filter_rows.push((
            row.clone().upcast(),
            format!("clean links tracking parameters {host}"),
        ));
    }

    group
}

fn update_preview(preview: &PreviewWidgets, rules: &UrlRules) {
    let link = preview.link.text();
    let link = link.trim();
    if link.is_empty() {
        preview.result.set_text("");
        preview.removed.set_visible(false);
        preview.error.set_subtitle("");
        preview.error.set_visible(false);
        preview.copy.set_sensitive(false);
        return;
    }
    match clean_url(link, rules) {
        Ok(cleaned) => {
            preview.result.set_text(&cleaned.cleaned);
            preview
                .removed
                .set_subtitle(&removed_lines(&cleaned.removed));
            preview.removed.set_visible(true);
            preview.error.set_subtitle("");
            preview.error.set_visible(false);
            preview.copy.set_sensitive(true);
        }
        Err(error) => {
            preview.result.set_text("");
            preview.removed.set_visible(false);
            preview.error.set_subtitle(match error.kind {
                UrlCleanupErrorKind::Unsupported => "Only http and https links can be cleaned.",
                UrlCleanupErrorKind::Malformed => "The link is not a well-formed address.",
            });
            preview.error.set_visible(true);
            preview.copy.set_sensitive(false);
        }
    }
}

fn removed_lines(removed: &[RemovedParam]) -> String {
    if removed.is_empty() {
        return "Nothing to remove".to_owned();
    }
    removed
        .iter()
        .map(|param| format!("{}={}", param.key, param.value))
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_params(text: &str) -> Vec<String> {
    text.split(',')
        .filter_map(|param| {
            let param = param.trim();
            (!param.is_empty()).then(|| param.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use kestrel_core::RemovedParam;

    use super::{parse_params, removed_lines};

    #[test]
    fn parameter_edits_trim_and_drop_blank_names() {
        assert_eq!(
            parse_params(" utm_* , fbclid ,, custom "),
            ["utm_*", "fbclid", "custom"]
        );
        assert!(parse_params("  , ").is_empty());
    }

    #[test]
    fn removed_parameters_list_one_key_value_per_line() {
        assert_eq!(removed_lines(&[]), "Nothing to remove");
        assert_eq!(
            removed_lines(&[
                RemovedParam {
                    key: "utm_source".to_owned(),
                    value: "news".to_owned(),
                },
                RemovedParam {
                    key: "ref".to_owned(),
                    value: String::new(),
                },
            ]),
            "utm_source=news\nref="
        );
    }
}
