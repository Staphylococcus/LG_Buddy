use std::cell::{Cell, RefCell};
#[cfg(test)]
use std::net::Ipv4Addr;
use std::rc::Rc;

use adw::prelude::*;
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
}

struct TvsMode {
    stack: gtk::Stack,
    status: adw::StatusPage,
    details: adw::PreferencesGroup,
    address: adw::ActionRow,
    mac: adw::ActionRow,
    input: adw::ActionRow,
    platform: adw::ActionRow,
    credentials: adw::ActionRow,
    credential_description: gtk::Label,
    retry: RetryButton,
}

impl TvsMode {
    fn new(on_intent: &IntentHandler) -> Self {
        let status = adw::StatusPage::builder()
            .icon_name(TV_ICON_NAME)
            .title("Loading TVs")
            .vexpand(true)
            .build();
        let retry = RetryButton::new(on_intent);
        status.set_child(Some(&retry.button));

        let details = adw::PreferencesGroup::builder().title("TV details").build();
        let address = detail_row("Address");
        let mac = detail_row("MAC address");
        let input = detail_row("HDMI input");
        let platform = detail_row("Platform");
        let credentials = detail_row("Credentials");
        for row in [&address, &mac, &input, &platform, &credentials] {
            details.add(row);
        }
        let credential_description = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .margin_start(12)
            .margin_end(12)
            .build();
        credential_description.add_css_class("dim-label");
        let details_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(16)
            .margin_bottom(20)
            .build();
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
            details,
            address,
            mac,
            input,
            platform,
            credentials,
            credential_description,
            retry,
        }
    }

    fn show_status(&self, title: &str, description: Option<&str>, error: bool) {
        self.status.set_title(title);
        self.status.set_description(description);
        self.status.set_icon_name(Some(TV_ICON_NAME));
        self.status.set_accessible_role(if error {
            gtk::AccessibleRole::Alert
        } else {
            gtk::AccessibleRole::Status
        });
        self.stack.set_visible_child_name("status");
    }

    fn show_details(&self, profile: &TvProfile) {
        self.details.set_title(profile.display_name());
        self.address.set_subtitle(&profile.address().to_string());
        self.mac.set_subtitle(&profile.mac().to_string());
        self.input.set_subtitle(profile.input_label());
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
        let multiple = TvsMode::new(&on_intent);
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

        let single = TvsMode::new(&on_intent);
        let root = gtk::Stack::new();
        root.set_vexpand(true);
        root.set_hexpand(true);
        root.set_hhomogeneous(false);
        root.set_vhomogeneous(false);
        root.add_named(&single.stack, Some("single"));
        root.add_named(&split_bin, Some("multiple"));
        root.set_visible_child_name("single");

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
        }
    }

    pub(crate) fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    pub(crate) fn render(&self, presentation: &TvsPresentation) {
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

        self.render_mode(&self.single, presentation);
        self.render_mode(&self.multiple, presentation);
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
        match presentation.status() {
            TvsStatus::Loading { message } => mode.show_status(message, None, false),
            TvsStatus::Empty { title, description } => {
                mode.show_status(title, Some(description), false)
            }
            TvsStatus::Failed(error) => {
                mode.show_status(error.summary(), Some(error.detail()), true)
            }
            TvsStatus::Ready => match presentation.selected_profile() {
                Some(profile) => mode.show_details(profile),
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
}

struct RetryButton {
    button: gtk::Button,
    intent: Rc<RefCell<Option<TvsIntent>>>,
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
    view.render(opening.presentation());
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
    view.render(empty.presentation());
    assert_eq!(view.single.status.title().as_str(), "No TV configured");
    assert_eq!(
        view.single.status.description().as_deref(),
        Some("Configure a TV to see its details here.")
    );

    let one = profile("primary", "Primary TV", "192.0.2.10");
    let (mut app, opening) = TvsApplication::open();
    let ready_one = app
        .complete_read(
            opening.read_operation().expect("loading operation"),
            Ok(vec![one]),
        )
        .expect("one profile transition");
    view.render(ready_one.presentation());
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
    assert!(
        intents.borrow().is_empty(),
        "rendering must not select a TV"
    );

    let model = app
        .complete_model_read(
            ready_one
                .model_read_operation()
                .expect("model read")
                .clone(),
            Ok("OLED42C2".to_string()),
        )
        .expect("model transition");
    view.render(model.presentation());
    assert_eq!(view.single.details.title().as_str(), "OLED42C2");
    assert_eq!(
        view.single.address.subtitle().as_deref(),
        Some("192.0.2.10")
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
    view.render(ready_multiple.presentation());
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
    view.render(selected.presentation());
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
    view.render(selected.presentation());
    assert!(
        !view.split.shows_content(),
        "unchanged selection preserves list navigation"
    );
    selected_row.emit_activate();
    assert!(view.split.shows_content());

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
    view.render(failed.presentation());
    assert!(view.single.retry.button.is_visible());
    view.single.retry.button.emit_clicked();
    assert_eq!(intents.borrow_mut().pop(), Some(TvsIntent::Retry));

    let display = view.root.display();
    assert!(gtk::IconTheme::for_display(&display).has_icon(TV_ICON_NAME));
    pump();

    // The TVs-local breakpoint collapses the split at narrow sizes while the
    // top-level application breakpoint remains owned by the window shell.
    view.render(ready_multiple.presentation());
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
}
