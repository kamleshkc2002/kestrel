//! Floating shelf: an undecorated surface beside the other windows.
//!
//! It stays open when another application takes focus, because the user drags
//! into it from there. Escape, the close button, `shelf.open` again, and a drag
//! that stays outside the surface close it. A layer-shell overlay is used where
//! the compositor offers one, anchored to the right edge and centred.

use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

use adw::prelude::*;
use async_channel::Sender;
use gtk::{Orientation, accessible::Property, gdk, glib};
use kestrel::{ApplicationCommand, view_model::ShelfViewModel};

use super::shelf::{ShelfDragEvent, ShelfDragHook, attach_shelf_drop_target, build_shelf_entries};
use super::widgets::turn_on_row;
use crate::layer_shell::{self, SurfacePlacement};

const SHELF_WIDTH: i32 = 360;
const SHELF_HEIGHT: i32 = 480;
/// How long a drag stays outside before the surface gets out of the way.
const DISMISS_DELAY: Duration = Duration::from_millis(400);

/// What the drag state machine asks the surface to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DragDismissAction {
    None,
    /// A pending dismissal no longer applies.
    Cancel,
    /// Start the dismissal delay.
    Schedule,
    /// Unmap until the drag ends; the drag keeps its data source.
    Hide,
    /// The drag is over; destroy the surface.
    Close,
}

/// Decides when a drag from our own rows has left the surface long enough.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DragDismiss {
    pinned: bool,
    own_drag: bool,
    hidden: bool,
    outside_since: Option<Instant>,
}

impl DragDismiss {
    /// Pinning keeps the surface open; unpinning changes nothing yet.
    pub fn set_pinned(&mut self, pinned: bool) -> DragDismissAction {
        self.pinned = pinned;
        if !pinned {
            return DragDismissAction::None;
        }
        self.outside_since = None;
        DragDismissAction::Cancel
    }

    pub fn drag_began(&mut self, _now: Instant) -> DragDismissAction {
        self.own_drag = true;
        self.hidden = false;
        self.outside_since = None;
        DragDismissAction::None
    }

    pub fn left(&mut self, now: Instant) -> DragDismissAction {
        if !self.own_drag || self.pinned || self.hidden {
            return DragDismissAction::None;
        }
        self.outside_since = Some(now);
        DragDismissAction::Schedule
    }

    pub fn entered(&mut self, _now: Instant) -> DragDismissAction {
        if !self.own_drag || self.hidden {
            return DragDismissAction::None;
        }
        self.outside_since = None;
        DragDismissAction::Cancel
    }

    pub fn timed_out(&mut self, now: Instant) -> DragDismissAction {
        if !self.own_drag || self.pinned || self.hidden {
            return DragDismissAction::None;
        }
        let Some(since) = self.outside_since else {
            return DragDismissAction::None;
        };
        if now.saturating_duration_since(since) < DISMISS_DELAY {
            return DragDismissAction::None;
        }
        self.outside_since = None;
        self.hidden = true;
        DragDismissAction::Hide
    }

    /// A drag from one of our rows is in progress.
    pub fn dragging(&self) -> bool {
        self.own_drag
    }

    pub fn drag_ended(&mut self, _now: Instant) -> DragDismissAction {
        self.own_drag = false;
        self.outside_since = None;
        if self.hidden {
            self.hidden = false;
            DragDismissAction::Close
        } else {
            DragDismissAction::None
        }
    }
}

/// The floating shelf's window and the state its drags follow.
pub struct ShelfSurface {
    pub window: gtk::Window,
    body: gtk::ScrolledWindow,
    commands: Sender<ApplicationCommand>,
    drag: Rc<RefCell<DragDismiss>>,
    dismiss_timer: Rc<RefCell<Option<glib::SourceId>>>,
    drag_hook: ShelfDragHook,
}

impl ShelfSurface {
    pub fn new(
        application: &adw::Application,
        parent: Option<&gtk::Window>,
        startup_id: Option<&str>,
        commands: &Sender<ApplicationCommand>,
        model: &ShelfViewModel,
    ) -> Self {
        let window = gtk::Window::builder()
            .application(application)
            .title("Shelf")
            .decorated(false)
            .resizable(false)
            .default_width(SHELF_WIDTH)
            .default_height(SHELF_HEIGHT)
            .build();
        window.add_css_class("background");
        match layer_shell::observed_placement() {
            SurfacePlacement::LayerShell { .. } => {
                layer_shell::configure_shelf_window(&window, "kestrel-shelf");
            }
            SurfacePlacement::Window { .. } => {
                if let Some(parent) = parent.filter(|parent| parent.is_mapped()) {
                    window.set_transient_for(Some(parent));
                }
            }
        }

        let drag = Rc::new(RefCell::new(DragDismiss::default()));
        let dismiss_timer = Rc::new(RefCell::new(None::<glib::SourceId>));
        let root = gtk::Box::new(Orientation::Vertical, 0);
        root.append(&header(
            &window,
            Rc::clone(&drag),
            Rc::clone(&dismiss_timer),
        ));

        let body = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .build();
        root.append(&body);

        let drag_hook: ShelfDragHook = {
            let drag = Rc::clone(&drag);
            let dismiss_timer = Rc::clone(&dismiss_timer);
            let weak_window = window.downgrade();
            Rc::new(move |event| match event {
                ShelfDragEvent::Began => {
                    cancel_timer(&dismiss_timer);
                    drag.borrow_mut().drag_began(Instant::now());
                }
                ShelfDragEvent::Ended => {
                    cancel_timer(&dismiss_timer);
                    let action = drag.borrow_mut().drag_ended(Instant::now());
                    if action == DragDismissAction::Close {
                        if let Some(window) = weak_window.upgrade() {
                            window.close();
                        }
                    }
                }
            })
        };

        let surface = Self {
            window,
            body,
            commands: commands.clone(),
            drag,
            dismiss_timer,
            drag_hook,
        };
        surface.attach_dismissal(&root);
        surface.set_shelf(model);
        attach_shelf_drop_target(&root, commands);
        surface.window.set_child(Some(&root));

        if let Some(startup_id) = startup_id {
            surface.window.set_startup_id(startup_id);
        }
        surface.window.present();
        surface
    }

    /// Whether a drag from one of the surface's rows is in progress.
    pub fn is_dragging(&self) -> bool {
        self.drag.borrow().dragging()
    }

    /// Rebuilds the list and drop area from the same view model as the page.
    pub fn set_shelf(&self, model: &ShelfViewModel) {
        let content = gtk::Box::new(Orientation::Vertical, 12);
        content.set_margin_top(8);
        content.set_margin_bottom(12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        if model.running {
            content.append(&build_shelf_entries(
                model,
                &self.commands,
                Some(&self.drag_hook),
            ));
        } else if let Some(turn_on) = &model.turn_on {
            content.append(&turn_on_row(turn_on, &self.commands));
        }
        self.body.set_child(Some(&content));
    }

    fn attach_dismissal(&self, root: &gtk::Box) {
        let escape = gtk::EventControllerKey::new();
        escape.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak_window = self.window.downgrade();
        escape.connect_key_pressed(move |_, key, _, _| {
            if key != gdk::Key::Escape {
                return glib::Propagation::Proceed;
            }
            if let Some(window) = weak_window.upgrade() {
                window.close();
            }
            glib::Propagation::Stop
        });
        self.window.add_controller(escape);

        let motion = gtk::DropControllerMotion::new();
        let drag = Rc::clone(&self.drag);
        let dismiss_timer = Rc::clone(&self.dismiss_timer);
        motion.connect_enter(move |_, _, _| {
            if drag.borrow_mut().entered(Instant::now()) == DragDismissAction::Cancel {
                cancel_timer(&dismiss_timer);
            }
        });
        let drag = Rc::clone(&self.drag);
        let dismiss_timer = Rc::clone(&self.dismiss_timer);
        let weak_window = self.window.downgrade();
        motion.connect_leave(move |_| {
            if drag.borrow_mut().left(Instant::now()) != DragDismissAction::Schedule {
                return;
            }
            start_timer(
                Rc::clone(&drag),
                Rc::clone(&dismiss_timer),
                weak_window.clone(),
            );
        });
        root.add_controller(motion);
    }
}

fn header(
    window: &gtk::Window,
    drag: Rc<RefCell<DragDismiss>>,
    dismiss_timer: Rc<RefCell<Option<glib::SourceId>>>,
) -> gtk::Box {
    let header = gtk::Box::new(Orientation::Horizontal, 6);
    header.set_margin_top(8);
    header.set_margin_bottom(4);
    header.set_margin_start(12);
    header.set_margin_end(8);
    let title = gtk::Label::new(Some("Shelf"));
    title.add_css_class("heading");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    header.append(&title);

    let pin = gtk::ToggleButton::new();
    pin.set_icon_name("view-pin-symbolic");
    pin.add_css_class("flat");
    pin.set_tooltip_text(Some("Keep open"));
    pin.update_property(&[Property::Label("Keep the shelf open")]);
    pin.connect_toggled(move |button| {
        if drag.borrow_mut().set_pinned(button.is_active()) == DragDismissAction::Cancel {
            cancel_timer(&dismiss_timer);
        }
        if button.is_active() {
            button.add_css_class("accent");
        } else {
            button.remove_css_class("accent");
        }
    });
    header.append(&pin);

    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.add_css_class("flat");
    close.set_tooltip_text(Some("Close the shelf"));
    close.update_property(&[Property::Label("Close the shelf")]);
    header.append(&close);

    let close_window = window.clone();
    close.connect_clicked(move |_| close_window.close());
    header
}

fn start_timer(
    drag: Rc<RefCell<DragDismiss>>,
    dismiss_timer: Rc<RefCell<Option<glib::SourceId>>>,
    window: glib::WeakRef<gtk::Window>,
) {
    cancel_timer(&dismiss_timer);
    let timer_slot = Rc::clone(&dismiss_timer);
    let id = glib::timeout_add_local_once(DISMISS_DELAY, move || {
        timer_slot.borrow_mut().take();
        if drag.borrow_mut().timed_out(Instant::now()) != DragDismissAction::Hide {
            return;
        }
        let Some(window) = window.upgrade() else {
            return;
        };
        // The drag keeps its data source after the origin surface unmaps; drag-end closes it.
        window.set_visible(false);
    });
    dismiss_timer.borrow_mut().replace(id);
}

fn cancel_timer(dismiss_timer: &Rc<RefCell<Option<glib::SourceId>>>) {
    if let Some(id) = dismiss_timer.borrow_mut().take() {
        id.remove();
    }
}

#[cfg(test)]
mod tests {
    use super::{DISMISS_DELAY, DragDismiss, DragDismissAction};
    use std::time::Instant;

    fn state() -> DragDismiss {
        DragDismiss::default()
    }

    #[test]
    fn a_drag_that_stays_outside_hides_after_the_delay() {
        let now = Instant::now();
        let mut dismiss = state();
        dismiss.drag_began(now);
        assert_eq!(
            dismiss.left(now + DISMISS_DELAY),
            DragDismissAction::Schedule
        );
        assert_eq!(
            dismiss.timed_out(now + DISMISS_DELAY + DISMISS_DELAY),
            DragDismissAction::Hide
        );
        assert_eq!(
            dismiss.drag_ended(now + DISMISS_DELAY + DISMISS_DELAY),
            DragDismissAction::Close
        );
    }

    #[test]
    fn coming_back_before_the_timeout_keeps_it_open() {
        let now = Instant::now();
        let mut dismiss = state();
        dismiss.drag_began(now);
        dismiss.left(now);
        assert_eq!(
            dismiss.entered(now + DISMISS_DELAY / 2),
            DragDismissAction::Cancel
        );
        assert_eq!(
            dismiss.timed_out(now + DISMISS_DELAY),
            DragDismissAction::None
        );
        assert_eq!(
            dismiss.drag_ended(now + DISMISS_DELAY),
            DragDismissAction::None
        );
    }

    #[test]
    fn a_pinned_surface_never_hides() {
        let now = Instant::now();
        let mut dismiss = state();
        dismiss.set_pinned(true);
        dismiss.drag_began(now);
        assert_eq!(dismiss.left(now), DragDismissAction::None);
        assert_eq!(
            dismiss.timed_out(now + DISMISS_DELAY),
            DragDismissAction::None
        );
    }

    #[test]
    fn a_drag_from_another_application_never_hides_it() {
        let now = Instant::now();
        let mut dismiss = state();
        assert_eq!(dismiss.left(now), DragDismissAction::None);
        assert_eq!(
            dismiss.timed_out(now + DISMISS_DELAY),
            DragDismissAction::None
        );
        assert_eq!(dismiss.drag_ended(now), DragDismissAction::None);
    }

    #[test]
    fn a_timeout_before_the_delay_keeps_the_drag_open() {
        let now = Instant::now();
        let mut dismiss = state();
        dismiss.drag_began(now);
        dismiss.left(now);
        assert_eq!(
            dismiss.timed_out(now + DISMISS_DELAY / 2),
            DragDismissAction::None
        );
        assert_eq!(
            dismiss.timed_out(now + DISMISS_DELAY),
            DragDismissAction::Hide
        );
    }
}
