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
    Toggle(gtk::Switch),
    Choice(gtk::DropDown),
    Number {
        entry: gtk::Entry,
        finalized: Rc<RefCell<String>>,
    },
}

struct NativeSettingRow {
    row: adw::ExpanderRow,
    value: gtk::Label,
    source: adw::ActionRow,
    problem: adw::ActionRow,
    feedback: adw::ActionRow,
    warning: gtk::Image,
    reset: gtk::Button,
    retry: gtk::Button,
    editor: NativeEditor,
    presentation: Rc<RefCell<SettingsRow>>,
    rendering: Rc<Cell<bool>>,
    restore_focus: Cell<bool>,
}

impl NativeSettingRow {
    fn new(initial: &SettingsRow, on_intent: Rc<dyn Fn(SettingsIntent)>) -> Self {
        let row = adw::ExpanderRow::builder()
            .use_markup(false)
            .title_lines(0)
            .subtitle_lines(0)
            .build();
        row.set_title(initial.title());
        row.set_subtitle(initial.description());
        let value = gtk::Label::builder()
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::Word)
            .width_chars(12)
            .max_width_chars(12)
            .xalign(1.0)
            .valign(gtk::Align::Center)
            .build();
        value.add_css_class("dim-label");
        row.add_suffix(&value);
        let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
        row.add_prefix(&warning);
        let presentation = Rc::new(RefCell::new(initial.clone()));
        let rendering = Rc::new(Cell::new(false));
        let editor_row = detail_row("Value", "");
        let editor = match initial.editor() {
            SettingsEditor::Toggle { .. } => {
                let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
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
                editor_row.add_suffix(&switch);
                editor_row.set_activatable_widget(Some(&switch));
                NativeEditor::Toggle(switch)
            }
            SettingsEditor::Choice { options, .. } => {
                let labels: Vec<_> = options.iter().map(|choice| choice.label()).collect();
                let dropdown = gtk::DropDown::from_strings(&labels);
                dropdown.set_valign(gtk::Align::Center);
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
                editor_row.add_suffix(&dropdown);
                NativeEditor::Choice(dropdown)
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
                editor_row.add_suffix(&entry);
                NativeEditor::Number { entry, finalized }
            }
        };
        row.add_row(&editor_row);
        let problem = detail_row("", "");
        problem.add_css_class("error");
        problem.set_accessible_role(gtk::AccessibleRole::Alert);
        row.add_row(&problem);
        let feedback = detail_row("", "");
        let retry = gtk::Button::builder()
            .label("Retry apply")
            .valign(gtk::Align::Center)
            .build();
        feedback.add_suffix(&retry);
        row.add_row(&feedback);
        let source = detail_row("Source", initial.source_label());
        row.add_row(&source);
        let default = detail_row("Default", initial.default_label());
        let reset = gtk::Button::builder()
            .label("Reset")
            // Let Reset replace a focused draft without a preceding focus-loss commit.
            .focus_on_click(false)
            .valign(gtk::Align::Center)
            .build();
        reset.update_property(&[gtk::accessible::Property::Label(&format!(
            "Reset {}",
            initial.title()
        ))]);
        default.add_suffix(&reset);
        row.add_row(&default);
        row.add_row(&detail_row(
            "Accepted values",
            initial.accepted_values_label(),
        ));
        for (button, is_retry) in [(&reset, false), (&retry, true)] {
            button.connect_clicked({
                let presentation = Rc::clone(&presentation);
                let on_intent = Rc::clone(&on_intent);
                move |_| {
                    let row = presentation.borrow().clone();
                    let action = if is_retry {
                        row.retry_apply_action()
                    } else {
                        row.reset_action()
                    };
                    if let Some(action) = action.filter(|action| action.enabled()) {
                        on_intent(action.intent());
                    }
                }
            });
        }
        let native = Self {
            row,
            value,
            source,
            problem,
            feedback,
            warning,
            reset,
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
        self.value.set_label(current.value_label());
        self.source.set_subtitle(current.source_label());
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
        if is_warning && current.edit_status() != previous.edit_status() {
            self.row.set_expanded(true);
        }
        for (button, action) in [
            (&self.reset, current.reset_action()),
            (&self.retry, current.retry_apply_action()),
        ] {
            button.set_visible(action.is_some());
            button.set_sensitive(action.is_some_and(|action| action.enabled()));
            if let Some(action) = action {
                button.set_label(action.label());
                // GtkButton labels itself from its child; use the setting-specific name instead.
                button.reset_relation(gtk::AccessibleRelation::LabelledBy);
                button.update_property(&[gtk::accessible::Property::Label(&format!(
                    "{} {}",
                    action.label(),
                    current.title()
                ))]);
            }
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
    rows[2].set_expanded(true);
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
    let writing = editing.handle_intent(intent).unwrap();
    view.render(writing.presentation());
    assert_eq!(
        entry.text(),
        "900",
        "progress must preserve the submitted draft"
    );
    assert!(!entry.is_sensitive());
    assert!(rows[2].is_expanded(), "progress preserves expansion");
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
    view.rows.borrow()[2].reset.emit_clicked();
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(SettingsIntent::Reset(BehaviorSetting::ScreenIdleTimeout))
    );
    entry.grab_focus();
    entry.set_text("720");
    view.rows.borrow()[2].reset.grab_focus();
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
    reset_pointer_click_discards_the_unfinalized_timeout(application);
}

#[cfg(test)]
fn reset_pointer_click_discards_the_unfinalized_timeout(application: &adw::Application) {
    use crate::controller_test_support::pump_until;
    use lg_buddy::settings::ConfigEnvReader;
    use lg_buddy::settings_view::{BehaviorSetting, SettingsApplication};
    use std::process::Command;

    let (mut model, opening) = SettingsApplication::open();
    let store =
        ConfigEnvReader::parse("/unused/config.env", "screen_idle_timeout=600\n").into_store();
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
        .title("LG Buddy Reset Pointer Test")
        .default_width(800)
        .default_height(900)
        .content(view.widget())
        .build();
    // Target the final row geometry, not a frame of the expansion animation.
    let settings = window.settings();
    let animations = settings.is_gtk_enable_animations();
    settings.set_gtk_enable_animations(false);
    window.present();
    let (entry, reset) = {
        let rows = view.rows.borrow();
        rows[2].row.set_expanded(true);
        let NativeEditor::Number { entry, .. } = &rows[2].editor else {
            unreachable!()
        };
        (entry.clone(), rows[2].reset.clone())
    };
    pump_until(|| reset.is_mapped() && reset.width() > 0);
    let search = Command::new("xdotool")
        .args([
            "search",
            "--onlyvisible",
            "--name",
            "^LG Buddy Reset Pointer Test$",
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
    entry.grab_focus();
    entry.set_text("900");
    assert!(intents.borrow().is_empty());
    let bounds = reset
        .compute_bounds(&window)
        .expect("Reset belongs to the test window");
    let x = (bounds.x() + bounds.width() / 2.0).round().to_string();
    let y = (bounds.y() + bounds.height() / 2.0).round().to_string();
    // XTest sends press/release events, exercising focus transfer before clicked.
    // emit_clicked() bypasses that ordering and cannot reproduce the lost Reset.
    assert!(Command::new("xdotool")
        .args(["mousemove", "--sync", "--window", id, &x, &y, "click", "1"])
        .status()
        .unwrap()
        .success());
    pump_until(|| !intents.borrow().is_empty());
    assert_eq!(
        *intents.borrow(),
        vec![SettingsIntent::Reset(BehaviorSetting::ScreenIdleTimeout)],
        "Reset must replace the draft without committing it first"
    );
    window.close();
    settings.set_gtk_enable_animations(animations);
}
