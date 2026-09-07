use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use lg_buddy::presentation::settings::{
    SettingsCommitPolicy, SettingsEditStatus, SettingsEditor, SettingsFeedbackSeverity,
    SettingsPresentation, SettingsRow, SettingsStatus,
};
use lg_buddy::settings_view::SettingsIntent;

/// Stable native controls render application-owned values and commit policies.
pub(crate) struct SettingsView {
    root: gtk::Stack,
    page: adw::PreferencesPage,
    groups: RefCell<Vec<adw::PreferencesGroup>>,
    rows: RefCell<Vec<NativeSettingRow>>,
    on_intent: Rc<dyn Fn(SettingsIntent)>,
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
        let retry_intent = Rc::new(RefCell::new(None::<SettingsIntent>));
        retry.connect_clicked({
            let intent = Rc::clone(&retry_intent);
            let on_intent = Rc::clone(&on_intent);
            move |_| {
                let intent = intent.borrow().clone();
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
            rows: RefCell::new(Vec::new()),
            on_intent,
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
                if self.rows.borrow().is_empty() {
                    for group in presentation.groups() {
                        let native = adw::PreferencesGroup::builder()
                            .title(gtk::glib::markup_escape_text(group.title()))
                            .description(gtk::glib::markup_escape_text(group.description()))
                            .build();
                        for row in group.rows() {
                            let row = NativeSettingRow::new(row, Rc::clone(&self.on_intent));
                            native.add(&row.row);
                            native.add(&row.problem);
                            native.add(&row.feedback);
                            self.rows.borrow_mut().push(row);
                        }
                        self.page.add(&native);
                        self.groups.borrow_mut().push(native);
                    }
                }
                for row in self.rows.borrow().iter() {
                    if let Some(presentation) = presentation
                        .groups()
                        .iter()
                        .flat_map(|group| group.rows())
                        .find(|value| value.setting() == row.presentation.borrow().setting())
                    {
                        row.render(presentation);
                    }
                }
                self.root.set_visible_child_name("settings");
            }
        }
    }
}

enum NativeEditor {
    Toggle(adw::SwitchRow),
    Choice(adw::ComboRow),
    Number {
        entry: gtk::Entry,
        finalized: Rc<RefCell<String>>,
    },
}

struct NativeSettingRow {
    row: adw::ActionRow,
    problem: adw::ActionRow,
    feedback: adw::ActionRow,
    warning: gtk::Image,
    retry: gtk::Button,
    editor: NativeEditor,
    presentation: Rc<RefCell<SettingsRow>>,
    rendering: Rc<Cell<bool>>,
    restore_focus: Cell<bool>,
}

impl NativeSettingRow {
    fn new(initial: &SettingsRow, on_intent: Rc<dyn Fn(SettingsIntent)>) -> Self {
        let presentation = Rc::new(RefCell::new(initial.clone()));
        let rendering = Rc::new(Cell::new(false));
        let (row, editor): (adw::ActionRow, NativeEditor) = match initial.editor() {
            SettingsEditor::Toggle { .. } => {
                let switch = adw::SwitchRow::new();
                switch.update_property(&[gtk::accessible::Property::Label(initial.title())]);
                switch.connect_active_notify({
                    let presentation = Rc::clone(&presentation);
                    let rendering = Rc::clone(&rendering);
                    let on_intent = Rc::clone(&on_intent);
                    move |switch| {
                        let row = presentation.borrow().clone();
                        if !rendering.get()
                            && row.editor_enabled()
                            && row.commit_policy() == SettingsCommitPolicy::OnChange
                        {
                            on_intent(SettingsIntent::SetEnabled {
                                setting: row.setting(),
                                enabled: switch.is_active(),
                            });
                        }
                    }
                });
                (switch.clone().upcast(), NativeEditor::Toggle(switch))
            }
            SettingsEditor::Choice { options, .. } => {
                let labels: Vec<_> = options.iter().map(|choice| choice.label()).collect();
                let dropdown = adw::ComboRow::builder()
                    .model(&gtk::StringList::new(&labels))
                    .build();
                // Preserve the native popup while keeping the selected value readable.
                dropdown.set_list_factory(dropdown.factory().as_ref());
                let factory = gtk::SignalListItemFactory::new();
                factory.connect_setup(|_, item| {
                    let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                    let label = gtk::Label::builder()
                        .wrap(true)
                        .wrap_mode(gtk::pango::WrapMode::Word)
                        .xalign(1.0)
                        .build();
                    label.add_css_class("dim-label");
                    item.set_child(Some(&label));
                });
                factory.connect_bind(|_, item| {
                    let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                    let value = item.item().and_downcast::<gtk::StringObject>().unwrap();
                    let label = item.child().and_downcast::<gtk::Label>().unwrap();
                    label.set_label(&value.string());
                });
                dropdown.set_factory(Some(&factory));
                dropdown.update_property(&[gtk::accessible::Property::Label(initial.title())]);
                dropdown.connect_selected_notify({
                    let presentation = Rc::clone(&presentation);
                    let rendering = Rc::clone(&rendering);
                    let on_intent = Rc::clone(&on_intent);
                    move |dropdown| {
                        let row = presentation.borrow().clone();
                        if !rendering.get()
                            && row.editor_enabled()
                            && row.commit_policy() == SettingsCommitPolicy::OnChange
                        {
                            if let Some(choice) = row
                                .editor()
                                .choices()
                                .and_then(|choices| choices.get(dropdown.selected() as usize))
                            {
                                on_intent(SettingsIntent::Commit {
                                    setting: row.setting(),
                                    value: choice.value().to_owned(),
                                });
                            }
                        }
                    }
                });
                (dropdown.clone().upcast(), NativeEditor::Choice(dropdown))
            }
            SettingsEditor::Number { text } => {
                let entry = gtk::Entry::builder()
                    .width_chars(8)
                    .max_width_chars(8)
                    .input_purpose(gtk::InputPurpose::Digits)
                    .valign(gtk::Align::Center)
                    .build();
                entry.update_property(&[
                    gtk::accessible::Property::Label(initial.title()),
                    gtk::accessible::Property::Description(
                        "Press Enter or leave this field to apply",
                    ),
                ]);
                let finalized = Rc::new(RefCell::new(text.clone()));
                let finalize: Rc<dyn Fn(&gtk::Entry)> = Rc::new({
                    let presentation = Rc::clone(&presentation);
                    let rendering = Rc::clone(&rendering);
                    let finalized = Rc::clone(&finalized);
                    let on_intent = Rc::clone(&on_intent);
                    move |entry| {
                        let row = presentation.borrow().clone();
                        let text = entry.text().to_string();
                        if !rendering.get()
                            && row.editor_enabled()
                            && row.commit_policy() == SettingsCommitPolicy::OnFinalize
                            && *finalized.borrow() != text
                        {
                            finalized.replace(text.clone());
                            on_intent(SettingsIntent::Commit {
                                setting: row.setting(),
                                value: text,
                            });
                        }
                    }
                });
                entry.connect_activate({
                    let finalize = Rc::clone(&finalize);
                    move |entry| finalize(entry)
                });
                let focus = gtk::EventControllerFocus::new();
                focus.connect_leave({
                    let entry = entry.downgrade();
                    move |_| {
                        if let Some(entry) = entry.upgrade() {
                            finalize(&entry);
                        }
                    }
                });
                entry.add_controller(focus);
                let row = adw::ActionRow::new();
                row.add_suffix(&entry);
                row.set_activatable_widget(Some(&entry));
                (row, NativeEditor::Number { entry, finalized })
            }
        };
        row.set_use_markup(false);
        row.set_title(initial.title());
        row.set_subtitle(initial.description());
        row.set_title_lines(0);
        row.set_subtitle_lines(0);
        let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
        row.add_prefix(&warning);
        let problem = detail_row("", "");
        problem.add_css_class("error");
        problem.set_accessible_role(gtk::AccessibleRole::Alert);
        let feedback = detail_row("", "");
        let retry = gtk::Button::builder()
            .label("Retry apply")
            .valign(gtk::Align::Center)
            .build();
        feedback.add_suffix(&retry);
        retry.connect_clicked({
            let presentation = Rc::clone(&presentation);
            move |_| {
                let row = presentation.borrow().clone();
                if let Some(action) = row.retry_apply_action().filter(|action| action.enabled()) {
                    on_intent(action.intent());
                }
            }
        });
        let native = Self {
            row,
            problem,
            feedback,
            warning,
            retry,
            editor,
            presentation,
            rendering,
            restore_focus: Cell::new(false),
        };
        // Populate values without producing commit intents.
        native.render_editor(initial.editor());
        native.render(initial);
        native
    }

    fn render_editor(&self, editor: &SettingsEditor) {
        self.rendering.set(true);
        match (&self.editor, editor) {
            (NativeEditor::Toggle(switch), SettingsEditor::Toggle { value }) => {
                switch.set_active(value.unwrap_or(false));
                switch.update_state(&[gtk::accessible::State::Invalid(if value.is_none() {
                    gtk::AccessibleInvalidState::True
                } else {
                    gtk::AccessibleInvalidState::False
                })]);
            }
            (NativeEditor::Choice(dropdown), SettingsEditor::Choice { selected, .. }) => {
                dropdown.set_selected(
                    selected.map_or(gtk::INVALID_LIST_POSITION, |value| value as u32),
                );
            }
            (NativeEditor::Number { entry, finalized }, SettingsEditor::Number { text }) => {
                entry.set_text(text);
                finalized.replace(text.clone());
            }
            _ => unreachable!("a behavior setting keeps its declared editor type"),
        }
        self.rendering.set(false);
    }

    fn render(&self, current: &SettingsRow) {
        let previous = self.presentation.borrow().clone();
        if !in_flight(current.edit_status())
            && (previous.editor() != current.editor() || in_flight(previous.edit_status()))
        {
            self.render_editor(current.editor());
        }
        self.rendering.set(true);
        let widget: &gtk::Widget = match &self.editor {
            NativeEditor::Toggle(widget) => widget.upcast_ref(),
            NativeEditor::Choice(widget) => widget.upcast_ref(),
            NativeEditor::Number { entry, .. } => entry.upcast_ref(),
        };
        if in_flight(current.edit_status()) && !in_flight(previous.edit_status()) {
            self.restore_focus.set(
                widget
                    .root()
                    .and_then(|root| root.downcast::<gtk::Window>().ok())
                    .and_then(|window| GtkWindowExt::focus(&window))
                    .is_some_and(|focus| focus == *widget || focus.is_ancestor(widget)),
            );
        }
        widget.set_sensitive(current.editor_enabled());
        self.row.set_sensitive(current.editor_enabled());
        self.row.update_property(&[
            gtk::accessible::Property::Label(current.title()),
            gtk::accessible::Property::Description(&format!(
                "{} {}",
                current.description(),
                current.value_label()
            )),
        ]);
        self.problem.set_visible(current.problem().is_some());
        self.problem.set_subtitle(current.problem().unwrap_or(""));
        self.problem
            .update_property(&[gtk::accessible::Property::Label(
                current.problem().unwrap_or(""),
            )]);
        let feedback = current.feedback();
        let message = feedback.map_or("", |feedback| feedback.message());
        let severity = feedback.map(|feedback| feedback.severity());
        let is_warning = matches!(
            severity,
            Some(SettingsFeedbackSeverity::Warning | SettingsFeedbackSeverity::Error)
        );
        self.feedback.set_visible(feedback.is_some());
        self.feedback.set_subtitle(message);
        self.feedback
            .update_property(&[gtk::accessible::Property::Label(message)]);
        self.feedback.set_accessible_role(if is_warning {
            gtk::AccessibleRole::Alert
        } else {
            gtk::AccessibleRole::Status
        });
        for (class, active) in [
            ("error", severity == Some(SettingsFeedbackSeverity::Error)),
            (
                "warning",
                severity == Some(SettingsFeedbackSeverity::Warning),
            ),
        ] {
            if active {
                self.feedback.add_css_class(class);
            } else {
                self.feedback.remove_css_class(class);
            }
        }
        self.warning
            .set_visible(current.problem().is_some() || is_warning);
        self.warning
            .set_tooltip_text(current.problem().or(is_warning.then_some(message)));
        let action = current.retry_apply_action();
        self.retry.set_visible(action.is_some());
        self.retry
            .set_sensitive(action.is_some_and(|action| action.enabled()));
        if let Some(action) = action {
            self.retry.set_label(action.label());
            // GtkButton labels itself from its child; use the setting-specific name instead.
            self.retry
                .reset_relation(gtk::AccessibleRelation::LabelledBy);
            self.retry
                .update_property(&[gtk::accessible::Property::Label(&format!(
                    "{} {}",
                    action.label(),
                    current.title()
                ))]);
        }
        self.presentation.replace(current.clone());
        if !in_flight(current.edit_status())
            && in_flight(previous.edit_status())
            && self.restore_focus.replace(false)
            && widget.is_mapped()
        {
            widget.grab_focus();
        }
        self.rendering.set(false);
    }
}

fn in_flight(status: SettingsEditStatus) -> bool {
    matches!(
        status,
        SettingsEditStatus::Validating
            | SettingsEditStatus::Persisting
            | SettingsEditStatus::Persisted
            | SettingsEditStatus::Applying
    )
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
    let rows: Vec<_> = view
        .rows
        .borrow()
        .iter()
        .map(|row| row.row.clone())
        .collect();
    assert!(!widgets.iter().any(|widget| widget.is::<adw::ExpanderRow>()));
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
        assert_eq!(row.subtitle().as_deref(), Some(declared.description()));
        assert!(!row.uses_markup());
    }
    assert_eq!(
        view.rows
            .borrow()
            .iter()
            .filter(|row| matches!(row.editor, NativeEditor::Toggle(_)))
            .count(),
        3
    );
    assert_eq!(
        view.rows
            .borrow()
            .iter()
            .filter(|row| matches!(row.editor, NativeEditor::Choice(_)))
            .count(),
        3
    );
    assert!(!widgets
        .iter()
        .filter_map(|widget| widget.clone().downcast::<gtk::Label>().ok())
        .any(|label| matches!(
            label.text().as_str(),
            "Source" | "Default" | "Accepted values" | "Reset"
        )));
    assert!(widgets
        .iter()
        .any(|widget| widget.accessible_role() == gtk::AccessibleRole::Alert));
    assert!(widgets
        .iter()
        .filter_map(|widget| widget.clone().downcast::<gtk::Label>().ok())
        .any(|label| label.text().contains("<invalid>&")));

    // Typing stays local until finalized; Enter followed by focus loss is one intent.
    use lg_buddy::settings_view::BehaviorSetting;
    let (mut editing, opening) = SettingsApplication::open();
    let ready = editing
        .complete_read(
            opening.read_operation().unwrap(),
            Ok(presentation.groups().to_vec()),
        )
        .unwrap();
    view.render(ready.presentation());
    intents.borrow_mut().clear();
    let entry = match &view.rows.borrow()[2].editor {
        NativeEditor::Number { entry, .. } => entry.clone(),
        _ => unreachable!(),
    };
    pump_until(|| entry.is_mapped());
    entry.grab_focus();
    entry.set_text("900");
    assert!(intents.borrow().is_empty());
    entry.emit_activate();
    assert_eq!(
        *intents.borrow(),
        vec![SettingsIntent::Commit {
            setting: BehaviorSetting::ScreenIdleTimeout,
            value: "900".into(),
        }]
    );
    let intent = intents.borrow_mut().pop().unwrap();
    let initial_height = view.groups.borrow()[0]
        .measure(gtk::Orientation::Vertical, 600)
        .1;
    let writing = editing.handle_intent(intent).unwrap();
    view.render(writing.presentation());
    for stage in [
        lg_buddy::settings::SettingsMutationStage::Validating,
        lg_buddy::settings::SettingsMutationStage::Persisting,
        lg_buddy::settings::SettingsMutationStage::Persisted,
        lg_buddy::settings::SettingsMutationStage::Applying,
    ] {
        let progress = editing
            .mutation_progress(writing.mutation_operation().unwrap(), stage)
            .unwrap();
        view.render(progress.presentation());
        assert!(!view.rows.borrow()[2].feedback.is_visible());
        assert_eq!(
            view.groups.borrow()[0]
                .measure(gtk::Orientation::Vertical, 600)
                .1,
            initial_height,
            "an ordinary setting change must not shift the layout"
        );
    }
    assert_eq!(
        entry.text(),
        "900",
        "progress must preserve the submitted draft"
    );
    assert!(!entry.is_sensitive());
    assert!(
        intents.borrow().is_empty(),
        "programmatic blur does not resubmit"
    );
    let failed = editing
        .complete_mutation(
            writing.mutation_operation().unwrap(),
            Err(lg_buddy::settings::SettingsMutationFailure::Persistence(
                lg_buddy::settings::SettingsError::Apply {
                    message: "write failed".into(),
                },
            )),
        )
        .unwrap();
    view.render(failed.presentation());
    assert_eq!(
        entry.text(),
        "600",
        "failure restores the previous effective value"
    );
    assert!(entry.is_sensitive());
    assert!(view.rows.borrow()[2].feedback.is_visible());
    assert!(intents.borrow().is_empty(), "restoration emits no write");
    match &view.rows.borrow()[1].editor {
        NativeEditor::Toggle(switch) => switch.set_active(false),
        _ => unreachable!(),
    }
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(SettingsIntent::SetEnabled {
            setting: BehaviorSetting::ScreenIdleBlank,
            enabled: false,
        })
    );
    match &view.rows.borrow()[6].editor {
        NativeEditor::Choice(choice) => choice.set_selected(1),
        _ => unreachable!(),
    }
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(SettingsIntent::Commit {
            setting: BehaviorSetting::UpdatesChannel,
            value: "prerelease".into(),
        })
    );
    entry.grab_focus();
    entry.set_text("720");
    rows[0].grab_focus();
    pump_until(|| !intents.borrow().is_empty());
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(SettingsIntent::Commit {
            setting: BehaviorSetting::ScreenIdleTimeout,
            value: "720".into(),
        })
    );

    window.set_default_size(360, 600);
    pump_until(|| window.width() <= 360);
    let (minimum, _, _, _) = view.widget().measure(gtk::Orientation::Horizontal, -1);
    assert!(
        minimum <= 360,
        "settings rows must fit a narrow window: {minimum}"
    );
    window.close();
    choice_row_click_opens_the_value_menu(application);
}

#[cfg(test)]
fn choice_row_click_opens_the_value_menu(application: &adw::Application) {
    use crate::controller_test_support::pump_until;
    use lg_buddy::settings::ConfigEnvReader;
    use lg_buddy::settings_view::{BehaviorSetting, SettingsApplication};
    use std::process::Command;

    let (mut model, opening) = SettingsApplication::open();
    let store = ConfigEnvReader::parse("/unused/config.env", "").into_store();
    let ready = model
        .complete_read(
            opening.read_operation().unwrap(),
            Ok(SettingsPresentation::from_store(&store).groups().to_vec()),
        )
        .unwrap();
    let model = RefCell::new(model);
    let intents = Rc::new(RefCell::new(Vec::new()));
    let view = Rc::new_cyclic(|renderer: &std::rc::Weak<SettingsView>| {
        let intents = Rc::clone(&intents);
        let renderer = renderer.clone();
        SettingsView::new(Rc::new(move |intent| {
            intents.borrow_mut().push(intent.clone());
            let transition = model.borrow_mut().handle_intent(intent);
            if let Some(transition) = transition {
                renderer
                    .upgrade()
                    .unwrap()
                    .render(transition.presentation());
            }
        }))
    });
    view.render(ready.presentation());
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("LG Buddy Choice Pointer Test")
        .default_width(800)
        .default_height(900)
        .content(view.widget())
        .build();
    window.present();
    let choice = match &view.rows.borrow()[0].editor {
        NativeEditor::Choice(choice) => choice.clone(),
        _ => unreachable!(),
    };
    pump_until(|| choice.is_mapped() && choice.width() > 0);
    let search = Command::new("xdotool")
        .args([
            "search",
            "--onlyvisible",
            "--name",
            "^LG Buddy Choice Pointer Test$",
        ])
        .output()
        .expect("xdotool is required for native pointer tests");
    assert!(search.status.success());
    let id = String::from_utf8(search.stdout).unwrap();
    let id = id.trim();
    assert!(Command::new("xdotool")
        .args(["windowfocus", "--sync", id])
        .status()
        .unwrap()
        .success());
    let bounds = choice
        .compute_bounds(&window)
        .expect("the choice row belongs to the test window");
    let x = (bounds.x() + bounds.width() / 2.0).round().to_string();
    let y = (bounds.y() + bounds.height() / 2.0).round().to_string();
    // Click the row body, not an inner dropdown control.
    assert!(Command::new("xdotool")
        .args(["mousemove", "--sync", "--window", id, &x, &y, "click", "1"])
        .status()
        .unwrap()
        .success());
    fn visible_popover(widget: &gtk::Widget) -> bool {
        if widget.is::<gtk::Popover>() && widget.is_mapped() {
            return true;
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if visible_popover(&current) {
                return true;
            }
            child = current.next_sibling();
        }
        false
    }
    pump_until(|| visible_popover(choice.upcast_ref()));
    assert!(intents.borrow().is_empty(), "opening choices must not save");
    assert!(Command::new("xdotool")
        .args(["key", "End", "Return"])
        .status()
        .unwrap()
        .success());
    pump_until(|| !intents.borrow().is_empty());
    let expected = ready.presentation().groups()[0].rows()[0]
        .editor()
        .choices()
        .unwrap()
        .last()
        .unwrap()
        .value();
    assert_eq!(
        *intents.borrow(),
        vec![SettingsIntent::Commit {
            setting: BehaviorSetting::ScreenBackend,
            value: expected.into(),
        }],
        "choosing a value must commit directly from the row"
    );
    window.close();
}
