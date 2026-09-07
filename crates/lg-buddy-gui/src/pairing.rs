use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use lg_buddy::config::HdmiInput;
use lg_buddy::pairing::PairingIntent;
use lg_buddy::presentation::brightness::UserFacingError;
use lg_buddy::presentation::pairing::PairingPresentation;
use lg_buddy::tvs::TvsIntent;

#[cfg(test)]
const TV_ICON_NAME: &str = "video-display-symbolic";

type IntentHandler = Rc<dyn Fn(TvsIntent)>;

/// GTK realization of the application-owned first-TV pairing presentation.
///
/// The renderer keeps the form values only as widget state. Every edit and
/// action is forwarded as a semantic pairing intent; validation and the
/// pairing workflow remain in the application layer.
pub(crate) struct PairingView {
    dialog: adw::Dialog,
    description: gtk::Label,
    address: adw::EntryRow,
    mac: adw::EntryRow,
    input: adw::ComboRow,
    status: gtk::Label,
    pair: gtk::Button,
    progress: gtk::ProgressBar,
    cancel: gtk::Button,
    toasts: adw::ToastOverlay,
    error_toast: RefCell<Option<adw::Toast>>,
    suppress: Rc<Cell<bool>>,
    presented: Cell<bool>,
}

impl PairingView {
    pub(crate) fn new(on_intent: IntentHandler) -> Self {
        let suppress = Rc::new(Cell::new(false));

        let description = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .build();
        description.set_accessible_role(gtk::AccessibleRole::Status);
        description.add_css_class("dim-label");

        let address = adw::EntryRow::builder()
            .title("TV address")
            .activates_default(true)
            .build();
        address.set_input_purpose(gtk::InputPurpose::FreeForm);
        connect_text_intent(
            &address,
            Rc::clone(&suppress),
            Rc::clone(&on_intent),
            PairingIntent::SetAddress,
        );

        let mac = adw::EntryRow::builder()
            .title("MAC address")
            .activates_default(true)
            .build();
        mac.set_input_purpose(gtk::InputPurpose::FreeForm);
        connect_text_intent(
            &mac,
            Rc::clone(&suppress),
            Rc::clone(&on_intent),
            PairingIntent::SetMac,
        );

        let input_model = gtk::StringList::new(&["HDMI 1", "HDMI 2", "HDMI 3", "HDMI 4"]);
        let input = adw::ComboRow::builder()
            .title("HDMI input")
            .model(&input_model)
            .build();
        input.set_selected(0);
        {
            let suppress = Rc::clone(&suppress);
            let on_intent = Rc::clone(&on_intent);
            input.connect_selected_notify(move |row| {
                if suppress.get() {
                    return;
                }
                if let Some(input) = hdmi_input(row.selected()) {
                    on_intent(TvsIntent::Pairing(PairingIntent::SetInput(input)));
                }
            });
        }

        let status = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .visible(false)
            .build();
        status.set_accessible_role(gtk::AccessibleRole::Alert);

        let pair = gtk::Button::with_label("Pair");
        pair.add_css_class("suggested-action");
        pair.connect_clicked({
            let on_intent = Rc::clone(&on_intent);
            move |_| on_intent(TvsIntent::Pairing(PairingIntent::Submit))
        });

        let cancel = gtk::Button::with_label("Cancel");
        cancel.connect_clicked({
            let on_intent = Rc::clone(&on_intent);
            move |_| on_intent(TvsIntent::Pairing(PairingIntent::Cancel))
        });

        let form = adw::PreferencesGroup::builder()
            .title("TV connection")
            .build();
        form.add(&address);
        form.add(&mac);
        form.add(&input);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(20)
            .margin_bottom(20)
            .build();
        content.append(&description);
        content.append(&status);
        content.append(&form);

        let clamp = adw::Clamp::builder()
            .maximum_size(600)
            .tightening_threshold(400)
            .margin_start(20)
            .margin_end(20)
            .child(&content)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .min_content_height(0)
            .propagate_natural_height(false)
            .child(&clamp)
            .build();
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        header.pack_start(&cancel);
        header.pack_end(&pair);
        let progress = gtk::ProgressBar::builder()
            .valign(gtk::Align::Start)
            .can_target(false)
            .visible(false)
            .build();
        progress.add_css_class("osd");
        let progress_overlay = gtk::Overlay::builder().child(&scroller).build();
        progress_overlay.add_overlay(&progress);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&progress_overlay));
        let toolbar = adw::ToolbarView::builder().content(&toasts).build();
        toolbar.add_top_bar(&header);
        let dialog = adw::Dialog::builder()
            .title("Pair a TV")
            .content_width(480)
            .content_height(480)
            .child(&toolbar)
            // Ask the application before dismissing: a worker may have begun
            // saving before its progress event reaches the main loop.
            .can_close(false)
            .build();
        dialog.connect_close_attempt({
            let on_intent = Rc::clone(&on_intent);
            let cancel = cancel.clone();
            move |_| {
                if cancel.is_sensitive() {
                    on_intent(TvsIntent::Pairing(PairingIntent::Cancel));
                }
            }
        });

        // Activating either text row submits the declared operation. The
        // button remains an ordinary action so Enter is not claimed globally.
        connect_submit_on_activate(&address, Rc::clone(&on_intent));
        connect_submit_on_activate(&mac, on_intent);

        Self {
            dialog,
            description,
            address,
            mac,
            input,
            status,
            pair,
            progress,
            cancel,
            toasts,
            error_toast: RefCell::new(None),
            suppress,
            presented: Cell::new(false),
        }
    }

    pub(crate) fn render(
        &self,
        parent: &adw::ApplicationWindow,
        presentation: Option<&PairingPresentation>,
    ) {
        let Some(presentation) = presentation else {
            self.clear_toast();
            self.progress.set_visible(false);
            if self.presented.replace(false) {
                self.dialog.force_close();
            }
            return;
        };
        self.suppress.set(true);
        self.dialog.set_title(presentation.title());
        self.description.set_text(presentation.description());
        set_entry_text_preserving_selection(&self.address, presentation.address());
        set_entry_text_preserving_selection(&self.mac, presentation.mac());
        if let Some(index) = hdmi_index(presentation.input()) {
            self.input.set_selected(index);
        }

        let editable = presentation.can_submit();
        self.address.set_sensitive(editable);
        self.mac.set_sensitive(editable);
        self.input.set_sensitive(editable);
        self.pair.set_sensitive(presentation.can_submit());
        self.progress
            .set_fraction(presentation.progress_fraction().unwrap_or(0.0));
        self.progress
            .set_visible(presentation.progress_fraction().is_some());
        self.progress
            .update_property(&[gtk::accessible::Property::Label(presentation.title())]);
        self.cancel.set_sensitive(presentation.can_cancel());

        if let Some(error) = presentation.error() {
            self.status.set_text(&error_text(error));
            self.status.add_css_class("error");
        } else {
            self.clear_toast();
            self.status.set_text("");
            self.status.remove_css_class("error");
        }
        self.status.set_visible(presentation.error().is_some());
        self.suppress.set(false);

        if !self.presented.replace(true) {
            self.dialog.present(Some(parent));
            self.address.grab_focus();
        }
    }

    pub(crate) fn show_toast(&self, message: &str) -> bool {
        if !self.presented.get() {
            return false;
        }
        self.clear_toast();
        let toast = adw::Toast::new(message);
        self.error_toast.replace(Some(toast.clone()));
        self.toasts.add_toast(toast);
        true
    }

    fn clear_toast(&self) {
        if let Some(toast) = self.error_toast.take() {
            toast.dismiss();
        }
    }
}

fn connect_text_intent<F>(
    row: &adw::EntryRow,
    suppress: Rc<Cell<bool>>,
    on_intent: IntentHandler,
    intent: F,
) where
    F: Fn(String) -> PairingIntent + 'static,
{
    row.connect_changed(move |row| {
        if !suppress.get() {
            let text = row.text().to_string();
            on_intent(TvsIntent::Pairing(intent(text)));
        }
    });
}

fn connect_submit_on_activate(row: &adw::EntryRow, on_intent: IntentHandler) {
    row.connect_entry_activated(move |_| {
        on_intent(TvsIntent::Pairing(PairingIntent::Submit));
    });
}

fn set_entry_text_preserving_selection(row: &adw::EntryRow, text: &str) {
    if row.text().as_str() == text {
        return;
    }
    let position = row.position();
    let selection = row.selection_bounds();
    row.set_text(text);
    let length = text.chars().count() as i32;
    if let Some((start, end)) = selection {
        row.select_region(start.min(length), end.min(length));
    } else {
        row.set_position(position.min(length));
    }
}

fn hdmi_input(index: u32) -> Option<HdmiInput> {
    match index {
        0 => Some(HdmiInput::Hdmi1),
        1 => Some(HdmiInput::Hdmi2),
        2 => Some(HdmiInput::Hdmi3),
        3 => Some(HdmiInput::Hdmi4),
        _ => None,
    }
}

fn hdmi_index(input: HdmiInput) -> Option<u32> {
    match input {
        HdmiInput::Hdmi1 => Some(0),
        HdmiInput::Hdmi2 => Some(1),
        HdmiInput::Hdmi3 => Some(2),
        HdmiInput::Hdmi4 => Some(3),
    }
}

fn error_text(error: &UserFacingError) -> String {
    format!("{} {}", error.summary(), error.detail())
}

#[cfg(test)]
pub(crate) fn run_renderer_scenarios(application: &adw::Application) {
    use std::cell::RefCell;

    use lg_buddy::config::HdmiInput;
    use lg_buddy::pairing::{PairingError, PairingFailure, PairingStage};
    use lg_buddy::tvs::TvsApplication;

    fn pump() {
        let context = gtk::glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }
    }

    let intents: Rc<RefCell<Vec<TvsIntent>>> = Rc::new(RefCell::new(Vec::new()));
    let view = PairingView::new(Rc::new({
        let intents = Rc::clone(&intents);
        move |intent| intents.borrow_mut().push(intent)
    }));
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .default_width(600)
        .default_height(420)
        .build();
    window.present();

    let (mut app, opening) = TvsApplication::open();
    let empty = app
        .complete_read(
            opening.read_operation().expect("TV read operation"),
            Ok(Vec::new()),
        )
        .expect("empty TVs transition");
    let pairing_opened = app
        .handle_intent(TvsIntent::PairTv)
        .expect("pairing transition");
    let presentation = pairing_opened
        .presentation()
        .pairing()
        .expect("pairing form");
    view.render(&window, Some(presentation));
    pump();
    assert_eq!(view.dialog.title(), "Pair a TV");
    assert_eq!(window.visible_dialog().as_ref(), Some(&view.dialog));
    assert_eq!(view.dialog.accessible_role(), gtk::AccessibleRole::Dialog);
    assert!(gtk::prelude::GtkWindowExt::focus(&window)
        .is_some_and(|focus| focus.is_ancestor(&view.address)));
    assert!(view.pair.is_sensitive());
    assert_eq!(view.pair.label().as_deref(), Some("Pair"));
    assert!(!view.progress.is_visible());
    assert!(view.cancel.is_sensitive());
    assert_eq!(
        gtk::prelude::GtkWindowExt::focus(&window)
            .unwrap()
            .accessible_role(),
        gtk::AccessibleRole::TextBox
    );
    assert_eq!(view.pair.accessible_role(), gtk::AccessibleRole::Button);

    view.address.set_text("192.0.2.42");
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(TvsIntent::Pairing(PairingIntent::SetAddress(
            "192.0.2.42".to_string()
        )))
    );
    view.mac.set_text("02:11:22:33:44:55");
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(TvsIntent::Pairing(PairingIntent::SetMac(
            "02:11:22:33:44:55".to_string()
        )))
    );
    view.input.set_selected(1);
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(TvsIntent::Pairing(PairingIntent::SetInput(
            HdmiInput::Hdmi2
        )))
    );

    let editing = app
        .handle_intent(TvsIntent::Pairing(PairingIntent::SetAddress(
            "192.0.2.42".to_string(),
        )))
        .expect("address edit");
    view.render(
        &window,
        Some(editing.presentation().pairing().expect("editing form")),
    );
    let editing = app
        .handle_intent(TvsIntent::Pairing(PairingIntent::SetMac(
            "02:11:22:33:44:55".to_string(),
        )))
        .expect("MAC edit");
    view.render(
        &window,
        Some(editing.presentation().pairing().expect("editing form")),
    );
    let editing = app
        .handle_intent(TvsIntent::Pairing(PairingIntent::SetInput(
            HdmiInput::Hdmi2,
        )))
        .expect("input edit");
    view.render(
        &window,
        Some(editing.presentation().pairing().expect("editing form")),
    );

    view.pair.emit_clicked();
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(TvsIntent::Pairing(PairingIntent::Submit))
    );
    let connecting = app
        .handle_intent(TvsIntent::Pairing(PairingIntent::Submit))
        .expect("pairing start");
    let operation = connecting
        .pairing_operation()
        .expect("pairing operation")
        .clone();
    view.render(
        &window,
        Some(connecting.presentation().pairing().expect("connecting")),
    );
    assert_eq!(view.dialog.title(), "Connecting to TV");
    assert!(!view.address.is_sensitive());
    assert!(!view.pair.is_sensitive());
    assert!(view.progress.is_visible());

    let waiting = app
        .pairing_progress(&operation, PairingStage::WaitingForConfirmation)
        .expect("confirmation transition");
    view.render(
        &window,
        Some(waiting.presentation().pairing().expect("confirmation")),
    );
    assert_eq!(view.dialog.title(), "Confirm on Your TV");
    assert_eq!(view.progress.fraction(), 0.25);
    assert!(view.progress.is_visible());
    assert!(view.description.label().contains("remote"));
    // Escape and sheet dismissal use the same intent as the header's Cancel.
    assert!(!view.dialog.close());
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(TvsIntent::Pairing(PairingIntent::Cancel))
    );
    let verifying = app
        .pairing_progress(&operation, PairingStage::Verifying)
        .expect("verification transition");
    view.render(
        &window,
        Some(verifying.presentation().pairing().expect("verification")),
    );

    let saving = app
        .pairing_progress(&operation, PairingStage::Saving)
        .expect("saving transition");
    view.render(&window, saving.presentation().pairing());
    assert_eq!(view.progress.fraction(), 0.75);
    assert!(view.progress.is_visible());
    assert!(!view.cancel.is_sensitive());
    intents.borrow_mut().clear();
    assert!(!view.dialog.close());
    assert!(intents.borrow().is_empty(), "saving cannot be dismissed");

    let failed = app
        .complete_pairing(&operation, Err(PairingError::new(PairingFailure::Rejected)))
        .expect("failed pairing transition");
    view.render(
        &window,
        Some(failed.presentation().pairing().expect("failed pairing")),
    );
    assert_eq!(view.dialog.title(), "Could Not Pair TV");
    assert!(!view.progress.is_visible());
    assert!(view.show_toast(failed.toast_message().expect("failure toast")));
    assert_eq!(
        view.error_toast
            .borrow()
            .as_ref()
            .unwrap()
            .title()
            .as_deref(),
        Some("Connection declined on TV")
    );
    assert!(view.status.is_visible());
    assert_eq!(view.status.accessible_role(), gtk::AccessibleRole::Alert);
    assert!(view.pair.is_sensitive());

    let editing = app
        .handle_intent(TvsIntent::Pairing(PairingIntent::SetAddress(
            "192.0.2.43".into(),
        )))
        .unwrap();
    view.render(&window, editing.presentation().pairing());
    assert!(view.error_toast.borrow().is_none());
    assert!(!view.status.is_visible());

    view.cancel.emit_clicked();
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(TvsIntent::Pairing(PairingIntent::Cancel))
    );
    let cancelled = app
        .handle_intent(TvsIntent::Pairing(PairingIntent::Cancel))
        .expect("pairing cancellation");
    assert!(cancelled.presentation().pairing().is_none());
    view.render(&window, cancelled.presentation().pairing());
    assert!(!view.presented.get());
    assert!(view.error_toast.borrow().is_none());
    assert!(!view.progress.is_visible());
    assert!(
        intents.borrow().is_empty(),
        "closing must not send a new intent"
    );

    let reopened = app.handle_intent(TvsIntent::PairTv).unwrap();
    view.render(&window, reopened.presentation().pairing());
    pump();
    assert!(view.presented.get());
    assert!(view.address.text().is_empty());
    assert_eq!(window.visible_dialog().as_ref(), Some(&view.dialog));
    view.render(&window, None);

    // The empty presentation owns the CTA; pairing owns the form instead.
    assert!(empty.presentation().pair_action().is_some());
    assert!(pairing_opened.presentation().pair_action().is_none());
    assert!(gtk::IconTheme::for_display(&view.dialog.display()).has_icon(TV_ICON_NAME));
    window.close();
}
