//! The first-launch setup dialog: welcome, a preset choice, and what the
//! chosen features use.

use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use async_channel::Sender;
use gtk::{Align, Orientation};
use kestrel::{
    ApplicationCommand, ApplicationViewModel, FeaturePreset, SetupPresetViewModel,
    SetupSensitiveViewModel, SetupViewModel,
};

/// Opens setup at the welcome step.
pub(crate) fn open_setup(
    window: &adw::ApplicationWindow,
    view_model: &ApplicationViewModel,
    commands: &Sender<ApplicationCommand>,
) {
    let choices = SetupViewModel::new(&view_model.features);
    let dialog = adw::Dialog::builder()
        .title("Set up Kestrel")
        .content_width(560)
        .content_height(560)
        .build();
    let navigation = adw::NavigationView::new();
    dialog.set_child(Some(&navigation));

    let preset_page = preset_step(&dialog, &navigation, commands, choices.presets);
    let welcome = welcome_step(&dialog, &navigation, commands, preset_page);
    navigation.push(&welcome);
    dialog.present(Some(window));
}

fn welcome_step(
    dialog: &adw::Dialog,
    navigation: &adw::NavigationView,
    commands: &Sender<ApplicationCommand>,
    preset_page: adw::NavigationPage,
) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .title("Welcome to Kestrel")
        .description(
            "Everyday utilities live in one window: system monitoring, audio, \
             clipboard history, screenshots, notes, and desktop integrations. They run as you, \
             with no root helpers and no telemetry, and every feature says what it needs \
             before it runs.",
        )
        .build();
    page.add(&group);

    let skip = gtk::Button::with_label("Skip setup");
    let dialog_for_skip = dialog.clone();
    let commands_for_skip = commands.clone();
    skip.connect_clicked(move |_| {
        let _ = commands_for_skip.try_send(ApplicationCommand::SkipSetup);
        dialog_for_skip.close();
    });

    let go_on = gtk::Button::with_label("Continue");
    go_on.add_css_class("suggested-action");
    let navigation_for_go = navigation.clone();
    go_on.connect_clicked(move |_| {
        navigation_for_go.push(&preset_page);
    });

    navigation_page("Welcome", &page, &footer_bar(&[&skip, &go_on]))
}

/// Preset choice. Each entry lists the features it turns on; the choice that
/// changes nothing skips the explanation step.
fn preset_step(
    dialog: &adw::Dialog,
    navigation: &adw::NavigationView,
    commands: &Sender<ApplicationCommand>,
    presets: Vec<SetupPresetViewModel>,
) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .title("Choose a preset")
        .description(
            "A preset turns a set of features on. You can change any of them later in the \
             Feature Hub.",
        )
        .build();
    let mut choices = Vec::with_capacity(presets.len());
    let mut first: Option<gtk::CheckButton> = None;
    for entry in presets {
        let check = gtk::CheckButton::new();
        check.set_valign(Align::Center);
        match &first {
            Some(first) => check.set_group(Some(first)),
            None => first = Some(check.clone()),
        }
        if entry.preset == Some(FeaturePreset::Balanced) {
            check.set_active(true);
        }
        let row = adw::ActionRow::builder()
            .title(entry.title)
            .subtitle(entry.detail.as_str())
            .subtitle_lines(0)
            .build();
        row.add_prefix(&check);
        row.set_activatable_widget(Some(&check));
        group.add(&row);
        choices.push((entry.preset, entry.sensitive, check));
    }
    page.add(&group);

    let go_on = gtk::Button::with_label("Continue");
    go_on.add_css_class("suggested-action");
    let choices = Rc::new(RefCell::new(choices));
    let dialog_for_go = dialog.clone();
    let navigation_for_go = navigation.clone();
    let commands_for_go = commands.clone();
    go_on.connect_clicked(move |_| {
        let selected = choices
            .borrow()
            .iter()
            .find(|(_, _, check)| check.is_active())
            .map(|(preset, sensitive, _)| (*preset, sensitive.clone()));
        let Some((preset, sensitive)) = selected else {
            return;
        };
        match preset {
            Some(preset) => navigation_for_go.push(&finish_step(
                &dialog_for_go,
                preset,
                sensitive,
                &commands_for_go,
            )),
            None => {
                let _ = commands_for_go.try_send(ApplicationCommand::FinishSetup {
                    preset: None,
                    left_off: Vec::new(),
                });
                dialog_for_go.close();
            }
        }
    });

    navigation_page("Choose a preset", &page, &footer_bar(&[&go_on]))
}

/// Explanation step. Every row starts on; switching one off leaves that
/// feature out of the preset.
fn finish_step(
    dialog: &adw::Dialog,
    preset: FeaturePreset,
    rows: Vec<SetupSensitiveViewModel>,
    commands: &Sender<ApplicationCommand>,
) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .title("Before you start")
        .description(
            "These features use something outside the app. Switch off any you do not want now; \
             you can turn them on later in the Feature Hub.",
        )
        .build();
    let switches = Rc::new(RefCell::new(Vec::<(String, gtk::Switch)>::new()));
    for row in rows {
        let line = adw::ActionRow::builder()
            .title(row.label.as_str())
            .subtitle(format!("{}\n{}", row.uses, row.keeps))
            .subtitle_lines(0)
            .build();
        let toggle = gtk::Switch::builder()
            .active(true)
            .valign(Align::Center)
            .build();
        line.add_suffix(&toggle);
        line.set_activatable_widget(Some(&toggle));
        group.add(&line);
        switches
            .borrow_mut()
            .push((row.feature_id.to_owned(), toggle));
    }
    if switches.borrow().is_empty() {
        group.set_description(Some(
            "This preset turns on no features that use anything outside the app.",
        ));
    }
    page.add(&group);

    let finish = gtk::Button::with_label("Finish");
    finish.add_css_class("suggested-action");
    let dialog_for_finish = dialog.clone();
    let commands_for_finish = commands.clone();
    finish.connect_clicked(move |_| {
        let left_off = switches
            .borrow()
            .iter()
            .filter(|(_, toggle)| !toggle.is_active())
            .map(|(feature_id, _)| feature_id.clone())
            .collect();
        let _ = commands_for_finish.try_send(ApplicationCommand::FinishSetup {
            preset: Some(preset),
            left_off,
        });
        dialog_for_finish.close();
    });

    navigation_page("Before you start", &page, &footer_bar(&[&finish]))
}

fn navigation_page(
    title: &str,
    content: &impl IsA<gtk::Widget>,
    footer: &gtk::Box,
) -> adw::NavigationPage {
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(content)
        .build();
    toolbar.set_content(Some(&scrolled));
    toolbar.add_bottom_bar(footer);
    adw::NavigationPage::builder()
        .title(title)
        .child(&toolbar)
        .build()
}

fn footer_bar(buttons: &[&gtk::Button]) -> gtk::Box {
    let bar = gtk::Box::new(Orientation::Horizontal, 6);
    bar.set_margin_top(12);
    bar.set_margin_bottom(12);
    bar.set_margin_start(12);
    bar.set_margin_end(12);
    bar.set_halign(Align::End);
    for button in buttons {
        bar.append(*button);
    }
    bar
}
