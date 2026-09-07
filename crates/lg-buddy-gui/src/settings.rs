use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use lg_buddy::presentation::settings::{SettingsPresentation, SettingsRow, SettingsStatus};
use lg_buddy::settings_view::SettingsIntent;

/// Native preference rows for the application-owned, read-only Settings view.
pub(crate) struct SettingsView {
    root: gtk::Stack,
    page: adw::PreferencesPage,
    groups: RefCell<Vec<adw::PreferencesGroup>>,
    status: adw::StatusPage,
    retry: gtk::Button,
    retry_intent: Rc<RefCell<Option<SettingsIntent>>>,
}

impl SettingsView {
    pub(crate) fn new(on_intent: Rc<dyn Fn(SettingsIntent)>) -> Self {
        let page = adw::PreferencesPage::new();
        let status = adw::StatusPage::builder()
            .icon_name("preferences-system-symbolic")
            .vexpand(true)
            .build();
        let retry = gtk::Button::builder().halign(gtk::Align::Center).build();
        retry.add_css_class("pill");
        let retry_intent = Rc::new(RefCell::new(None));
        retry.connect_clicked({
            let intent = Rc::clone(&retry_intent);
            move |_| {
                let intent = *intent.borrow();
                if let Some(intent) = intent {
                    on_intent(intent);
                }
            }
        });
        status.set_child(Some(&retry));
        let root = gtk::Stack::builder()
            .hexpand(true)
            .vexpand(true)
            .hhomogeneous(false)
            .vhomogeneous(false)
            .build();
        root.add_named(&status, Some("status"));
        root.add_named(&page, Some("settings"));
        root.set_visible_child_name("status");
        Self {
            root,
            page,
            groups: RefCell::new(Vec::new()),
            status,
            retry,
            retry_intent,
        }
    }

    pub(crate) fn widget(&self) -> &gtk::Stack {
        &self.root
    }

    pub(crate) fn render(&self, presentation: &SettingsPresentation) {
        let action = presentation.retry_action();
        self.retry.set_visible(action.is_some());
        self.retry
            .set_sensitive(action.is_some_and(|action| action.enabled()));
        self.retry
            .set_label(action.map_or("", |action| action.label()));
        self.retry_intent
            .replace(action.map(|action| action.intent()));
        match presentation.status() {
            SettingsStatus::Loading { message } => {
                self.status.set_title(message);
                self.status.set_description(None);
                self.root.set_visible_child_name("status");
            }
            SettingsStatus::Failed(error) => {
                self.status.set_title(error.summary());
                self.status.set_description(Some(error.detail()));
                self.root.set_visible_child_name("status");
            }
            SettingsStatus::Ready => {
                for group in self.groups.borrow_mut().drain(..) {
                    self.page.remove(&group);
                }
                for group in presentation.groups() {
                    let native = adw::PreferencesGroup::builder()
                        .title(gtk::glib::markup_escape_text(group.title()))
                        .description(gtk::glib::markup_escape_text(group.description()))
                        .build();
                    for row in group.rows() {
                        native.add(&setting_row(row));
                    }
                    self.page.add(&native);
                    self.groups.borrow_mut().push(native);
                }
                self.root.set_visible_child_name("settings");
            }
        }
    }
}

fn setting_row(presentation: &SettingsRow) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder()
        .use_markup(false)
        .title_lines(0)
        .subtitle_lines(0)
        .build();
    // Set text after construction so native child labels already use plain text.
    row.set_title(presentation.title());
    row.set_subtitle(presentation.description());
    let value = gtk::Label::builder()
        .label(presentation.value_label())
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::Word)
        .width_chars(12)
        .max_width_chars(12)
        .xalign(1.0)
        .valign(gtk::Align::Center)
        .build();
    value.add_css_class("dim-label");
    row.add_suffix(&value);
    row.update_property(&[
        gtk::accessible::Property::Label(presentation.title()),
        gtk::accessible::Property::Description(&format!(
            "{} {}",
            presentation.description(),
            presentation.value_label()
        )),
    ]);
    if let Some(problem) = presentation.problem() {
        let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
        icon.add_css_class("error");
        icon.set_tooltip_text(Some(problem));
        row.add_prefix(&icon);
        let error = detail_row("", problem);
        error.add_css_class("error");
        error.set_accessible_role(gtk::AccessibleRole::Alert);
        error.update_property(&[gtk::accessible::Property::Label(problem)]);
        row.add_row(&error);
    }
    for (title, value) in [
        ("Source", presentation.source_label()),
        ("Default", presentation.default_label()),
        ("Accepted values", presentation.accepted_values_label()),
    ] {
        row.add_row(&detail_row(title, value));
    }
    row
}

fn detail_row(title: &str, value: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .subtitle_lines(0)
        .activatable(false)
        .selectable(false)
        .build();
    row.set_title(title);
    row.set_subtitle(value);
    row
}

#[cfg(test)]
pub(crate) fn run_renderer_scenarios(application: &adw::Application) {
    use crate::controller_test_support::pump_until;
    use lg_buddy::settings::{ConfigEnvReader, SettingsStore};
    use lg_buddy::settings_view::{SettingsApplication, SettingsReadError};

    fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
        let mut found = vec![widget.clone()];
        let mut child = widget.first_child();
        while let Some(current) = child {
            found.extend(descendants(&current));
            child = current.next_sibling();
        }
        found
    }

    let intents = Rc::new(RefCell::new(Vec::new()));
    let view = SettingsView::new(Rc::new({
        let intents = Rc::clone(&intents);
        move |intent| intents.borrow_mut().push(intent)
    }));
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .default_width(1100)
        .default_height(800)
        .content(view.widget())
        .build();
    let (mut model, opening) = SettingsApplication::open();
    view.render(opening.presentation());
    window.present();
    pump_until(|| view.status.is_mapped());
    assert_eq!(view.root.visible_child_name().as_deref(), Some("status"));
    assert!(!view.retry.is_visible());
    let failed = model
        .complete_read(
            opening.read_operation().unwrap(),
            Err(SettingsReadError::stopped()),
        )
        .unwrap();
    view.render(failed.presentation());
    assert!(view.retry.is_visible());
    view.retry.emit_clicked();
    assert_eq!(*intents.borrow(), vec![SettingsIntent::Retry]);

    let store = SettingsStore::from_reader(ConfigEnvReader::parse(
        "/unused/config.env",
        "screen_idle_timeout=600\nupdates_channel=<invalid>&\n",
    ));
    let presentation = SettingsPresentation::from_store(&store);
    view.render(&presentation);
    pump_until(|| view.page.is_mapped() && view.groups.borrow()[0].width() > 0);
    let widgets = descendants(view.widget().upcast_ref());
    let rows: Vec<adw::ExpanderRow> = widgets
        .iter()
        .filter_map(|widget| widget.clone().downcast().ok())
        .collect();
    assert_eq!(view.groups.borrow().len(), 3);
    assert_eq!(rows.len(), 7);
    assert!(widgets
        .iter()
        .filter_map(|widget| widget.clone().downcast::<gtk::Label>().ok())
        .any(|label| label.text() == "Sleep & Wake"));
    assert!(
        view.groups.borrow()[0].width() < 800,
        "preference content must be clamped in wide windows"
    );
    for label in widgets
        .iter()
        .filter_map(|widget| widget.clone().downcast::<gtk::Label>().ok())
        .filter(|label| {
            matches!(
                label.text().as_str(),
                "Automatic" | "Enabled" | "Conservative"
            )
        })
    {
        assert_eq!(
            label.layout().line_count(),
            1,
            "setting values must not wrap within a word"
        );
    }
    for (row, declared) in rows
        .iter()
        .zip(presentation.groups().iter().flat_map(|group| group.rows()))
    {
        assert_eq!(row.title(), declared.title());
        assert_eq!(row.subtitle(), declared.description());
        assert!(!row.uses_markup());
        assert!(!row.shows_enable_switch());
    }
    assert!(!widgets
        .iter()
        .any(|widget| widget.is::<gtk::Switch>() && widget.is_visible()));
    assert!(!widgets
        .iter()
        .any(|widget| widget.is::<adw::ComboRow>() || widget.is::<gtk::Entry>()));
    rows[0].set_expanded(true);
    pump_until(|| rows[0].is_expanded());
    let first_details = descendants(rows[0].upcast_ref());
    assert!(first_details
        .iter()
        .filter_map(|widget| widget.clone().downcast::<adw::ActionRow>().ok())
        .any(|row| row.title() == "Accepted values"));
    assert!(widgets
        .iter()
        .any(|widget| widget.accessible_role() == gtk::AccessibleRole::Alert));
    assert!(widgets
        .iter()
        .filter_map(|widget| widget.clone().downcast::<gtk::Label>().ok())
        .any(|label| label.text().contains("<invalid>&")));

    window.set_default_size(360, 600);
    pump_until(|| window.width() <= 360);
    let (minimum, _, _, _) = view.widget().measure(gtk::Orientation::Horizontal, -1);
    assert!(
        minimum <= 360,
        "settings rows must fit a narrow window: {minimum}"
    );
    window.close();
}
