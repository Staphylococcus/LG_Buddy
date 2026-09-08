use std::cell::{Cell, RefCell};
#[cfg(test)]
use std::net::Ipv4Addr;
use std::rc::Rc;

use adw::prelude::*;
use lg_buddy::config::HdmiInput;
use lg_buddy::presentation::tvs::{TvsAction, TvsPresentation, TvsStatus};
use lg_buddy::tvs::{TvId, TvProfile, TvsIntent};

const TV_ICON_NAME: &str = "video-display-symbolic";

type IntentHandler = Rc<dyn Fn(TvsIntent)>;

/// GTK realization of the application-owned TVs presentation.
///
/// The renderer keeps no selected profile of its own.  A list-row event only
/// forwards `TvsIntent::Select`; the next presentation decides which details
/// are visible.
pub(crate) struct TvsView {
    root: gtk::Stack,
    single: TvsMode,
    multiple: TvsMode,
    split: adw::NavigationSplitView,
    #[cfg(test)]
    split_bin: adw::BreakpointBin,
    sidebar: gtk::ListBox,
    sidebar_ids: Rc<RefCell<Vec<TvId>>>,
    sidebar_rows: RefCell<Vec<adw::ActionRow>>,
    rendered_selected: RefCell<Option<TvId>>,
    suppress: Rc<Cell<bool>>,
    unpair_dialog: adw::AlertDialog,
    unpair_dialog_visible: Cell<bool>,
    unpair_confirm_intent: Rc<RefCell<Option<TvsIntent>>>,
    unpair_cancel_intent: Rc<RefCell<Option<TvsIntent>>>,
    restore_focus: RefCell<Option<gtk::Widget>>,
}

struct TvsMode {
    stack: gtk::Stack,
    status: adw::StatusPage,
    status_error: gtk::Label,
    pair: PairButton,
    details: adw::PreferencesGroup,
    address: adw::ActionRow,
    mac: adw::ActionRow,
    input: adw::ComboRow,
    platform: adw::ActionRow,
    credentials: adw::ActionRow,
    credential_description: gtk::Label,
    unpair: UnpairButton,
    management_error: gtk::Label,
    management_actions: gtk::Box,
    management_retry: RetryButton,
    retry: RetryButton,
}

impl TvsMode {
    fn new(on_intent: &IntentHandler, suppress: &Rc<Cell<bool>>) -> Self {
        let status = adw::StatusPage::builder()
            .icon_name(TV_ICON_NAME)
            .title("Loading TVs")
            .vexpand(true)
            .build();
        let retry = RetryButton::new(on_intent);
        let pair = PairButton::new(on_intent);
        let status_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        status_actions.set_halign(gtk::Align::Center);
        status_actions.append(&retry.button);
        status_actions.append(&pair.button);
        let status_error = gtk::Label::builder().wrap(true).visible(false).build();
        status_error.set_accessible_role(gtk::AccessibleRole::Alert);
        status_error.add_css_class("error");
        let status_content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        status_content.append(&status_error);
        status_content.append(&status_actions);
        status.set_child(Some(&status_content));

        let details = adw::PreferencesGroup::builder().title("TV details").build();
        let address = detail_row("Address");
        let mac = detail_row("MAC address");
        let input_model = gtk::StringList::new(&["HDMI 1", "HDMI 2", "HDMI 3", "HDMI 4"]);
        let input = adw::ComboRow::builder()
            .title("HDMI input")
            .model(&input_model)
            .build();
        input.set_selected(0);
        {
            let suppress = Rc::clone(suppress);
            let on_intent = Rc::clone(on_intent);
            input.connect_selected_notify(move |row| {
                if suppress.get() {
                    return;
                }
                if let Some(input) = hdmi_input(row.selected()) {
                    on_intent(TvsIntent::SetInput(input));
                }
            });
        }
        let platform = detail_row("Platform");
        let credentials = detail_row("Credentials");
        details.add(&address);
        details.add(&mac);
        details.add(&input);
        details.add(&platform);
        details.add(&credentials);
        let credential_description = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .margin_start(12)
            .margin_end(12)
            .build();
        credential_description.add_css_class("dim-label");
        let unpair = UnpairButton::new(on_intent);
        details.set_header_suffix(Some(&unpair.button));
        let management_error = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .visible(false)
            .build();
        management_error.set_accessible_role(gtk::AccessibleRole::Alert);
        management_error.add_css_class("error");
        let management_retry = RetryButton::new(on_intent);
        let management_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        management_actions.append(&management_error);
        management_actions.append(&management_retry.button);
        let details_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(16)
            .margin_bottom(20)
            .build();
        details_box.append(&management_actions);
        details_box.append(&details);
        details_box.append(&credential_description);
        let clamp = adw::Clamp::builder()
            .maximum_size(600)
            .tightening_threshold(400)
            .margin_start(20)
            .margin_end(20)
            .child(&details_box)
            .build();
        let details_scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&clamp)
            .build();

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.set_hexpand(true);
        stack.set_hhomogeneous(false);
        stack.set_vhomogeneous(false);
        stack.add_named(&status, Some("status"));
        stack.add_named(&details_scroller, Some("details"));
        stack.set_visible_child_name("status");

        Self {
            stack,
            status,
            status_error,
            pair,
            details,
            address,
            mac,
            input,
            platform,
            credentials,
            credential_description,
            unpair,
            management_error,
            management_actions,
            management_retry,
            retry,
        }
    }

    fn show_status(&self, title: &str, description: Option<&str>, error: bool) {
        self.status.set_title(title);
        self.status
            .set_description(if error { None } else { description });
        self.status.set_icon_name(Some(TV_ICON_NAME));
        self.status_error.set_text(if error {
            description.unwrap_or(title)
        } else {
            ""
        });
        self.status_error.set_visible(error);
        self.stack.set_visible_child_name("status");
    }

    fn show_details(&self, profile: &TvProfile) {
        self.details.set_title(profile.display_name());
        self.address.set_subtitle(&profile.address().to_string());
        self.mac.set_subtitle(&profile.mac().to_string());
        self.platform.set_subtitle(profile.platform_label());
        self.credentials.set_subtitle(profile.credentials().label());
        self.credential_description
            .set_text(profile.credentials().description());
        self.stack.set_visible_child_name("details");
    }
}

impl TvsView {
    pub(crate) fn new(on_intent: Rc<dyn Fn(TvsIntent)>) -> Self {
        let suppress = Rc::new(Cell::new(false));
        let sidebar_ids = Rc::new(RefCell::new(Vec::new()));
        let sidebar = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .show_separators(false)
            .build();
        sidebar.add_css_class("boxed-list");
        sidebar.set_margin_start(12);
        sidebar.set_margin_end(12);
        sidebar.set_margin_top(12);
        sidebar.set_margin_bottom(12);
        sidebar.set_accessible_role(gtk::AccessibleRole::List);
        sidebar.connect_row_selected({
            let suppress = Rc::clone(&suppress);
            let sidebar_ids = Rc::clone(&sidebar_ids);
            let on_intent = Rc::clone(&on_intent);
            move |_, row| {
                if suppress.get() {
                    return;
                }
                let Some(row) = row else {
                    return;
                };
                let index = row.index();
                if index < 0 {
                    return;
                }
                let id = sidebar_ids.borrow().get(index as usize).cloned();
                if let Some(id) = id {
                    on_intent(TvsIntent::Select(id));
                }
            }
        });

        let sidebar_toolbar = toolbar_page("TVs", &sidebar, false);
        let sidebar_page = adw::NavigationPage::builder()
            .title("TVs")
            .child(&sidebar_toolbar)
            .build();
        let multiple = TvsMode::new(&on_intent, &suppress);
        let multiple_toolbar = toolbar_page("TV details", &multiple.stack, true);
        let content_page = adw::NavigationPage::builder()
            .title("TV details")
            .child(&multiple_toolbar)
            .build();
        let split = adw::NavigationSplitView::builder()
            .sidebar(&sidebar_page)
            .content(&content_page)
            .show_content(true)
            .build();
        split.set_min_sidebar_width(220.0);
        split.set_max_sidebar_width(300.0);
        split.set_sidebar_width_fraction(0.32);

        // Keep this breakpoint local to the TVs view.  The application window
        // owns a separate breakpoint for the top-level tabs.
        let split_bin = adw::BreakpointBin::builder()
            .child(&split)
            .vexpand(true)
            .hexpand(true)
            .width_request(1)
            .height_request(1)
            .build();
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            700.0,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        split_bin.add_breakpoint(narrow);

        let split_for_activation = split.clone();
        sidebar.connect_row_activated(move |_, _| {
            // A second activation of the already-selected row does not reach
            // the application because the selection did not change.  It must
            // still open the details page in a collapsed split view.
            split_for_activation.set_show_content(true);
        });

        let single = TvsMode::new(&on_intent, &suppress);
        let root = gtk::Stack::new();
        root.set_vexpand(true);
        root.set_hexpand(true);
        root.set_hhomogeneous(false);
        root.set_vhomogeneous(false);
        root.add_named(&single.stack, Some("single"));
        root.add_named(&split_bin, Some("multiple"));
        root.set_visible_child_name("single");

        let unpair_dialog = adw::AlertDialog::builder()
            .can_close(true)
            .default_response("cancel")
            .close_response("cancel")
            .build();
        unpair_dialog.add_responses(&[("cancel", ""), ("confirm", "")]);
        unpair_dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
        let unpair_confirm_intent = Rc::new(RefCell::new(None));
        let unpair_cancel_intent = Rc::new(RefCell::new(None));
        unpair_dialog.connect_response(None, {
            let on_intent = Rc::clone(&on_intent);
            let unpair_confirm_intent = unpair_confirm_intent.clone();
            let unpair_cancel_intent = unpair_cancel_intent.clone();
            move |_, response| {
                let intent = match response {
                    "confirm" => unpair_confirm_intent.borrow().clone(),
                    "cancel" => unpair_cancel_intent.borrow().clone(),
                    _ => None,
                };
                if let Some(intent) = intent {
                    on_intent(intent);
                }
            }
        });

        Self {
            root,
            single,
            multiple,
            split,
            #[cfg(test)]
            split_bin,
            sidebar,
            sidebar_ids,
            sidebar_rows: RefCell::new(Vec::new()),
            rendered_selected: RefCell::new(None),
            suppress,
            unpair_dialog,
            unpair_dialog_visible: Cell::new(false),
            unpair_confirm_intent,
            unpair_cancel_intent,
            restore_focus: RefCell::new(None),
        }
    }

    pub(crate) fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    pub(crate) fn render(&self, parent: &adw::ApplicationWindow, presentation: &TvsPresentation) {
        if presentation.unpair_confirmation().is_some() && !self.unpair_dialog_visible.get() {
            self.restore_focus
                .replace(gtk::prelude::GtkWindowExt::focus(parent));
        }
        self.suppress.set(true);
        let profiles = presentation.profiles();
        let is_multiple = profiles.len() > 1;
        self.root
            .set_visible_child_name(if is_multiple { "multiple" } else { "single" });

        if is_multiple {
            self.render_sidebar(profiles, presentation.selected_profile());
        } else {
            self.sidebar.unselect_all();
        }

        if is_multiple {
            self.render_mode(&self.multiple, presentation);
        } else {
            self.render_mode(&self.single, presentation);
        }
        self.render_unpair_dialog(parent, presentation);
        self.suppress.set(false);
    }

    fn render_sidebar(&self, profiles: &[TvProfile], selected: Option<&TvProfile>) {
        let selected_id = selected.map(|profile| profile.id().clone());
        let selection_changed = *self.rendered_selected.borrow() != selected_id;
        self.rendered_selected.replace(selected_id.clone());

        let incoming_ids: Vec<TvId> = profiles
            .iter()
            .map(|profile| profile.id().clone())
            .collect();
        let ids_changed = *self.sidebar_ids.borrow() != incoming_ids;
        if ids_changed {
            for row in self.sidebar_rows.borrow().iter() {
                self.sidebar.remove(row);
            }
            let mut rows = self.sidebar_rows.borrow_mut();
            rows.clear();
            for profile in profiles {
                let row = sidebar_row(profile);
                self.sidebar.append(&row);
                rows.push(row);
            }
        }
        self.sidebar_ids.replace(incoming_ids);

        let mut selected_row = None;
        for (index, profile) in profiles.iter().enumerate() {
            if selected_id
                .as_ref()
                .is_some_and(|selected| selected == profile.id())
            {
                selected_row = Some(index);
            }
            if let Some(row) = self.sidebar_rows.borrow().get(index) {
                row.set_title(profile.display_name());
                row.set_subtitle(&profile.address().to_string());
            }
        }
        if let Some(index) = selected_row {
            if let Some(row) = self.sidebar.row_at_index(index as i32) {
                self.sidebar.select_row(Some(&row));
            }
        } else {
            self.sidebar.unselect_all();
        }
        if selection_changed && selected_row.is_some() {
            self.split.set_show_content(true);
        }
    }

    fn render_mode(&self, mode: &TvsMode, presentation: &TvsPresentation) {
        mode.retry.render(presentation.retry_action());
        mode.pair.render(presentation.pair_action());
        mode.unpair.render(presentation.unpair_action());
        mode.management_retry
            .render(presentation.retry_apply_action());
        let management_error = presentation.management_error();
        if let Some(error) = management_error {
            mode.management_error.set_text(&error_text(error));
        } else {
            mode.management_error.set_text("");
        }
        mode.management_error
            .set_visible(management_error.is_some());
        mode.management_actions
            .set_visible(management_error.is_some() || presentation.retry_apply_action().is_some());
        match presentation.status() {
            TvsStatus::Loading { message } => mode.show_status(message, None, false),
            TvsStatus::Empty { title, description } => {
                mode.show_status(title, Some(description), false)
            }
            TvsStatus::Failed(error) => {
                mode.show_status(error.summary(), Some(error.detail()), true)
            }
            TvsStatus::Ready => match presentation.selected_profile() {
                Some(profile) => {
                    mode.input.set_sensitive(presentation.input_enabled());
                    let selected = hdmi_index(profile.input());
                    if mode.input.selected() != selected {
                        mode.input.set_selected(selected);
                    }
                    mode.show_details(profile)
                }
                None => {
                    debug_assert!(
                        false,
                        "a Ready TVs presentation must declare its selected profile"
                    );
                    mode.stack.set_visible_child_name("details");
                }
            },
        }
    }

    fn render_unpair_dialog(
        &self,
        parent: &adw::ApplicationWindow,
        presentation: &TvsPresentation,
    ) {
        let Some(confirmation) = presentation.unpair_confirmation() else {
            self.unpair_confirm_intent.replace(None);
            self.unpair_cancel_intent.replace(None);
            if self.unpair_dialog_visible.replace(false) {
                self.unpair_dialog.force_close();
                if let Some(focus) = self.restore_focus.take() {
                    let _ = focus.grab_focus();
                }
            }
            return;
        };

        self.unpair_dialog.set_heading(Some(confirmation.title()));
        self.unpair_dialog.set_body(confirmation.body());
        self.unpair_dialog
            .set_response_label("confirm", confirmation.confirm_action().label());
        self.unpair_dialog
            .set_response_label("cancel", confirmation.cancel_action().label());
        self.unpair_dialog
            .set_response_enabled("confirm", confirmation.confirm_action().enabled());
        self.unpair_dialog
            .set_response_enabled("cancel", confirmation.cancel_action().enabled());
        self.unpair_confirm_intent
            .replace(Some(confirmation.confirm_action().intent()));
        self.unpair_cancel_intent
            .replace(Some(confirmation.cancel_action().intent()));

        if !self.unpair_dialog_visible.replace(true) {
            self.unpair_dialog.present(Some(parent));
        }
    }
}

struct RetryButton {
    button: gtk::Button,
    intent: Rc<RefCell<Option<TvsIntent>>>,
}

struct PairButton {
    button: gtk::Button,
    intent: Rc<RefCell<Option<TvsIntent>>>,
}

struct UnpairButton {
    button: gtk::Button,
    intent: Rc<RefCell<Option<TvsIntent>>>,
}

impl UnpairButton {
    fn new(on_intent: &IntentHandler) -> Self {
        crate::register_resources();
        let icon = gtk::gio::FileIcon::new(&gtk::gio::File::for_uri(
            "resource:///io/github/staphylococcus/LGBuddy/icons/edit-delete-symbolic.svg",
        ));
        let button = gtk::Button::builder()
            .child(&gtk::Image::from_gicon(&icon))
            .width_request(36)
            .height_request(36)
            .valign(gtk::Align::Center)
            .visible(false)
            .sensitive(false)
            .build();
        button.add_css_class("flat");
        button.add_css_class("circular");
        button.add_css_class("destructive-action");
        let intent = Rc::new(RefCell::new(None));
        button.connect_clicked({
            let on_intent = Rc::clone(on_intent);
            let intent = Rc::clone(&intent);
            move |_| {
                let intent = intent.borrow().clone();
                if let Some(intent) = intent {
                    on_intent(intent);
                }
            }
        });
        Self { button, intent }
    }

    fn render(&self, action: Option<&TvsAction>) {
        self.intent.replace(
            action
                .filter(|action| action.enabled())
                .map(TvsAction::intent),
        );
        self.button.set_visible(action.is_some());
        self.button
            .set_sensitive(action.is_some_and(TvsAction::enabled));
        self.button.set_tooltip_text(action.map(TvsAction::label));
        self.button
            .update_property(&[gtk::accessible::Property::Label(
                action.map_or("", TvsAction::label),
            )]);
    }
}

impl PairButton {
    fn new(on_intent: &IntentHandler) -> Self {
        let button = gtk::Button::with_label("Pair a TV");
        button.add_css_class("suggested-action");
        button.add_css_class("pill");
        button.set_visible(false);
        button.set_sensitive(false);
        let intent = Rc::new(RefCell::new(None));
        button.connect_clicked({
            let on_intent = Rc::clone(on_intent);
            let intent = Rc::clone(&intent);
            move |_| {
                let intent = intent.borrow().clone();
                if let Some(intent) = intent {
                    on_intent(intent);
                }
            }
        });
        Self { button, intent }
    }

    fn render(&self, action: Option<&TvsAction>) {
        let was_visible = self.button.is_visible();
        self.intent.replace(
            action
                .filter(|action| action.enabled())
                .map(TvsAction::intent),
        );
        self.button.set_visible(action.is_some());
        self.button
            .set_sensitive(action.is_some_and(TvsAction::enabled));
        self.button
            .set_label(action.map_or("Pair a TV", TvsAction::label));
        self.button.set_tooltip_text(action.map(TvsAction::label));
        self.button
            .update_property(&[gtk::accessible::Property::Label(
                action.map_or("Pair a TV", TvsAction::label),
            )]);
        if action.is_some() && !was_visible && !self.button.has_focus() {
            let button = self.button.clone();
            gtk::glib::idle_add_local_once(move || {
                if button.is_mapped() && button.is_visible() && button.is_sensitive() {
                    button.grab_focus();
                }
            });
        }
    }
}

impl RetryButton {
    fn new(on_intent: &IntentHandler) -> Self {
        let button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .visible(false)
            .sensitive(false)
            .build();
        button.add_css_class("flat");
        let intent = Rc::new(RefCell::new(None));
        button.connect_clicked({
            let on_intent = Rc::clone(on_intent);
            let intent = Rc::clone(&intent);
            move |_| {
                let intent = intent.borrow().clone();
                if let Some(intent) = intent {
                    on_intent(intent);
                }
            }
        });
        Self { button, intent }
    }

    fn render(&self, action: Option<&TvsAction>) {
        self.intent.replace(
            action
                .filter(|action| action.enabled())
                .map(TvsAction::intent),
        );
        self.button.set_visible(action.is_some());
        self.button
            .set_sensitive(action.is_some_and(TvsAction::enabled));
        self.button.set_tooltip_text(action.map(TvsAction::label));
        self.button
            .update_property(&[gtk::accessible::Property::Label(
                action.map_or("", TvsAction::label),
            )]);
    }
}

fn detail_row(title: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle("")
        .activatable(false)
        .selectable(false)
        .build()
}

fn error_text(error: &lg_buddy::presentation::brightness::UserFacingError) -> String {
    format!("{} {}", error.summary(), error.detail())
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

fn hdmi_index(input: HdmiInput) -> u32 {
    match input {
        HdmiInput::Hdmi1 => 0,
        HdmiInput::Hdmi2 => 1,
        HdmiInput::Hdmi3 => 2,
        HdmiInput::Hdmi4 => 3,
    }
}

fn sidebar_row(profile: &TvProfile) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(profile.display_name())
        .subtitle(profile.address().to_string())
        .activatable(true)
        .selectable(true)
        .build()
}

fn toolbar_page(
    title: &str,
    content: &impl IsA<gtk::Widget>,
    show_back_button: bool,
) -> adw::ToolbarView {
    let header = adw::HeaderBar::builder()
        .show_back_button(show_back_button)
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    let title = adw::WindowTitle::new(title, "");
    header.set_title_widget(Some(&title));
    let toolbar = adw::ToolbarView::builder().content(content).build();
    toolbar.add_top_bar(&header);
    toolbar
}

#[cfg(test)]
pub(crate) fn run_renderer_scenarios(application: &adw::Application) {
    use crate::controller_test_support::pump_until;
    use lg_buddy::config::{HdmiInput, TvPlatform};
    use lg_buddy::tvs::{TvCredentialState, TvsApplication, TvsReadError, TvsReadFailure};

    fn pump() {
        let context = gtk::glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }
    }

    fn profile(id: &str, name: &str, address: &str) -> TvProfile {
        TvProfile::new(
            id,
            name,
            address.parse::<Ipv4Addr>().expect("fixture address"),
            "aa:bb:cc:dd:ee:ff".parse().expect("fixture MAC address"),
            HdmiInput::Hdmi2,
            TvPlatform::LgWebOs,
            TvCredentialState::Stored,
        )
    }

    let intents: Rc<RefCell<Vec<TvsIntent>>> = Rc::new(RefCell::new(Vec::new()));
    let view = TvsView::new(Rc::new({
        let intents = Rc::clone(&intents);
        move |intent| intents.borrow_mut().push(intent)
    }));
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .default_width(600)
        .default_height(420)
        .content(view.widget())
        .build();
    window.present();
    let (mut app, opening) = TvsApplication::open();
    view.render(&window, opening.presentation());
    assert_eq!(view.root.visible_child_name().as_deref(), Some("single"));
    assert_eq!(
        view.single.status.icon_name().as_deref(),
        Some(TV_ICON_NAME)
    );

    let empty = app
        .complete_read(
            opening.read_operation().expect("loading operation"),
            Ok(Vec::new()),
        )
        .expect("empty transition");
    view.render(&window, empty.presentation());
    assert_eq!(view.single.status.title().as_str(), "No TV configured");
    assert_eq!(
        view.single.status.description().as_deref(),
        Some("Pair your TV to control it with LG Buddy.")
    );
    assert!(view.single.pair.button.is_visible());
    assert!(view.single.pair.button.is_sensitive());
    assert_eq!(
        view.single.pair.button.label().as_deref(),
        Some("Pair a TV")
    );
    pump();
    assert!(view.single.pair.button.has_focus());

    view.single.pair.button.emit_clicked();
    assert_eq!(intents.borrow_mut().pop(), Some(TvsIntent::PairTv));
    let pairing = app
        .handle_intent(TvsIntent::PairTv)
        .expect("pairing transition");
    view.render(&window, pairing.presentation());
    assert_eq!(
        view.single.stack.visible_child_name().as_deref(),
        Some("status")
    );
    assert!(!view.single.pair.button.is_visible());
    let blank = app
        .handle_intent(TvsIntent::Pairing(lg_buddy::pairing::PairingIntent::Cancel))
        .expect("blank transition");
    view.render(&window, blank.presentation());
    pump();
    assert_eq!(
        view.single.stack.visible_child_name().as_deref(),
        Some("status")
    );
    assert!(view.single.pair.button.has_focus());

    let one = profile("primary", "Primary TV", "192.0.2.10");
    let (mut app, opening) = TvsApplication::open();
    let ready_one = app
        .complete_read(
            opening.read_operation().expect("loading operation"),
            Ok(vec![one]),
        )
        .expect("one profile transition");
    view.render(&window, ready_one.presentation());
    assert_eq!(view.root.visible_child_name().as_deref(), Some("single"));
    assert_eq!(
        view.single.stack.visible_child_name().as_deref(),
        Some("details")
    );
    pump();
    assert!(view.single.stack.is_mapped());
    assert!(!view.split.is_mapped(), "one TV has no visible sidebar");
    assert_eq!(view.single.details.title().as_str(), "Primary TV");
    assert_eq!(
        view.single.address.subtitle().as_deref(),
        Some("192.0.2.10")
    );
    assert!(view
        .single
        .mac
        .subtitle()
        .is_some_and(|subtitle| subtitle.contains("aa:bb:cc:dd:ee:ff")));
    assert!(view
        .single
        .credentials
        .subtitle()
        .is_some_and(|subtitle| subtitle.contains("Stored locally")));
    assert_eq!(view.single.input.selected(), 1);
    assert!(view.single.input.is_sensitive());
    assert!(
        intents.borrow().is_empty(),
        "rendering must not select a TV"
    );

    // A user change is semantic input, while a later presentation update
    // restores the optimistic/effective value without feeding back an intent.
    view.single.input.set_selected(2);
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(TvsIntent::SetInput(HdmiInput::Hdmi3))
    );
    view.render(&window, ready_one.presentation());
    assert_eq!(view.single.input.selected(), 1);
    assert!(intents.borrow().is_empty(), "render must not apply input");

    let model = app
        .complete_model_read(
            ready_one
                .model_read_operation()
                .expect("model read")
                .clone(),
            Ok("OLED42C2".to_string()),
        )
        .expect("model transition");
    view.render(&window, model.presentation());
    assert_eq!(view.single.details.title().as_str(), "OLED42C2");
    assert_eq!(
        view.single.address.subtitle().as_deref(),
        Some("192.0.2.10")
    );

    let pending = app
        .handle_intent(TvsIntent::SetInput(HdmiInput::Hdmi3))
        .expect("input transition");
    view.render(&window, pending.presentation());
    assert!(view.single.input.is_sensitive());
    assert!(!view.single.unpair.button.is_sensitive());
    view.single.input.set_selected(3);
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(TvsIntent::SetInput(HdmiInput::Hdmi4))
    );

    let (mut app, opening) = TvsApplication::open();
    let ready_multiple = app
        .complete_read(
            opening.read_operation().expect("loading operation"),
            Ok(vec![
                profile("first", "First TV", "192.0.2.11"),
                profile("second", "Second TV", "192.0.2.12"),
            ]),
        )
        .expect("multiple profile transition");
    view.render(&window, ready_multiple.presentation());
    assert_eq!(view.root.visible_child_name().as_deref(), Some("multiple"));
    assert_eq!(view.sidebar_rows.borrow().len(), 2);
    assert_eq!(view.sidebar.selected_row().map(|row| row.index()), Some(0));
    assert!(
        intents.borrow().is_empty(),
        "rendering must not select a TV"
    );

    let second_row = view.sidebar.row_at_index(1).expect("second TV row");
    view.sidebar.select_row(Some(&second_row));
    let selected_intent = intents.borrow_mut().pop().expect("selection intent");
    assert_eq!(selected_intent, TvsIntent::Select("second".into()));
    let selected = app
        .handle_intent(selected_intent)
        .expect("changed selection transition");
    view.render(&window, selected.presentation());
    assert!(
        intents.borrow().is_empty(),
        "render must not feed back selection"
    );

    view.split.set_collapsed(true);
    view.split.set_show_content(false);
    let selected_row = view.sidebar.row_at_index(1).expect("selected TV row");
    selected_row.emit_activate();
    assert!(
        view.split.shows_content(),
        "activating a selected row opens details"
    );

    view.multiple
        .stack
        .activate_action("navigation.pop", None)
        .expect("native Back action");
    assert!(!view.split.shows_content(), "Back opens the TV list");
    assert!(window.is_visible(), "Back must not close the application");
    view.render(&window, selected.presentation());
    assert!(
        !view.split.shows_content(),
        "unchanged selection preserves list navigation"
    );
    selected_row.emit_activate();
    assert!(view.split.shows_content());

    pump();
    assert!(
        view.multiple.unpair.button.grab_focus(),
        "unpair action must be focusable"
    );
    view.multiple.unpair.button.emit_clicked();
    assert_eq!(intents.borrow_mut().pop(), Some(TvsIntent::UnpairTv));
    let confirming = app
        .handle_intent(TvsIntent::UnpairTv)
        .expect("confirmation transition");
    view.render(&window, confirming.presentation());
    assert_eq!(view.unpair_dialog.heading().as_deref(), Some("Unpair TV?"));
    assert_eq!(
        view.unpair_dialog.default_response().as_deref(),
        Some("cancel")
    );
    assert_eq!(
        window.visible_dialog().as_ref(),
        Some(view.unpair_dialog.upcast_ref::<adw::Dialog>())
    );
    // libadwaita 1.5 opens the sheet on frame-clock ticks. Closing it before
    // its contents are mapped has no effect, even though visible_dialog is set.
    pump_until(|| {
        view.unpair_dialog
            .child()
            .is_some_and(|child| child.is_mapped())
    });
    assert!(view.unpair_dialog.close());
    pump_until(|| !intents.borrow().is_empty());
    assert_eq!(intents.borrow_mut().pop(), Some(TvsIntent::CancelUnpair));
    let cancelled = app
        .handle_intent(TvsIntent::CancelUnpair)
        .expect("cancel transition");
    view.render(&window, cancelled.presentation());
    pump_until(|| window.visible_dialog().is_none());
    assert!(window.visible_dialog().is_none());
    assert!(
        gtk::prelude::GtkWindowExt::focus(&window).is_some_and(|focus| focus
            == view.multiple.unpair.button.clone().upcast::<gtk::Widget>()
            || focus.is_ancestor(&view.multiple.unpair.button)),
        "cancel restores focus to the destructive action"
    );

    let (mut app, opening) = TvsApplication::open();
    let failed = app
        .complete_read(
            opening.read_operation().expect("loading operation"),
            Err(TvsReadError::new(
                TvsReadFailure::Internal,
                "fixture failure",
            )),
        )
        .expect("failure transition");
    view.render(&window, failed.presentation());
    assert!(view.single.retry.button.is_visible());
    view.single.retry.button.emit_clicked();
    assert_eq!(intents.borrow_mut().pop(), Some(TvsIntent::Retry));

    let display = view.root.display();
    assert!(gtk::IconTheme::for_display(&display).has_icon(TV_ICON_NAME));
    pump();

    // The TVs-local breakpoint collapses the split at narrow sizes while the
    // top-level application breakpoint remains owned by the window shell.
    view.render(&window, ready_multiple.presentation());
    pump();
    view.split.set_collapsed(false);
    view.split_bin.allocate(900, 420, -1, None);
    assert_eq!(view.split_bin.width(), 900);
    assert!(view.split_bin.current_breakpoint().is_none());
    assert!(!view.split.is_collapsed());
    view.split_bin.allocate(420, 420, -1, None);
    assert_eq!(view.split_bin.width(), 420);
    assert!(view.split_bin.current_breakpoint().is_some());
    assert!(view.split.is_collapsed());
    window.close();
    crate::pairing::run_renderer_scenarios(application);
}
