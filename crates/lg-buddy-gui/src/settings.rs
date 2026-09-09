use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use lg_buddy::presentation::settings::{
    SettingsCommitPolicy, SettingsEditStatus, SettingsEditor, SettingsFeedbackSeverity,
    SettingsPresentation, SettingsRow, SettingsStatus,
};
use lg_buddy::presentation::update_check::UpdateCheckPresentation;
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
    update_check: UpdateCheckView,
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
        let update_check = UpdateCheckView::new(Rc::clone(&on_intent));
        Self {
            root,
            page,
            groups: RefCell::new(Vec::new()),
            rows: RefCell::new(Vec::new()),
            on_intent,
            status,
            retry,
            retry_intent,
            update_check,
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
            SettingsStatus::Loading { .. } if !presentation.groups().is_empty() => {
                self.render_rows(presentation);
                self.root.set_visible_child_name("settings");
            }
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
                        if group.title() == "Updates" {
                            self.update_check.attach(&native);
                        }
                        self.page.add(&native);
                        self.groups.borrow_mut().push(native);
                    }
                }
                self.render_rows(presentation);
                self.root.set_visible_child_name("settings");
            }
        }
    }

    fn render_rows(&self, presentation: &SettingsPresentation) {
        for row in self.rows.borrow().iter() {
            if let Some(current) = presentation
                .groups()
                .iter()
                .flat_map(|group| group.rows())
                .find(|value| value.setting() == row.presentation.borrow().setting())
            {
                row.render(current, presentation.row_visible(current.setting()));
            }
        }
        self.update_check.render(presentation.update_check());
    }
}

struct UpdateCheckView {
    installed: adw::ActionRow,
    check: gtk::Button,
    check_intent: Rc<RefCell<Option<SettingsIntent>>>,
    spinner: gtk::Spinner,
    result: adw::ActionRow,
    release: gtk::LinkButton,
    error: adw::ActionRow,
    warning: adw::ActionRow,
}

impl UpdateCheckView {
    fn new(on_intent: Rc<dyn Fn(SettingsIntent)>) -> Self {
        let installed = detail_row("Installed version", "");
        let check = gtk::Button::with_label("Check for updates");
        check.add_css_class("suggested-action");
        check.set_valign(gtk::Align::Center);
        check.update_property(&[gtk::accessible::Property::Label("Check for updates")]);
        let check_intent = Rc::new(RefCell::new(None));
        check.connect_clicked({
            let check_intent = Rc::clone(&check_intent);
            move |_| {
                let intent = check_intent.borrow().clone();
                if let Some(intent) = intent {
                    on_intent(intent);
                }
            }
        });
        let spinner = gtk::Spinner::new();
        spinner.set_valign(gtk::Align::Center);
        spinner.set_visible(false);
        spinner.update_property(&[gtk::accessible::Property::Label("Checking for updates")]);
        installed.add_suffix(&check);
        installed.add_suffix(&spinner);

        let result = detail_row("", "");
        result.set_accessible_role(gtk::AccessibleRole::Status);
        result.set_visible(false);
        let release = gtk::LinkButton::builder()
            .label("View release")
            .uri("https://github.com/Staphylococcus/LG_Buddy/releases")
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        release.update_property(&[gtk::accessible::Property::Label("View available release")]);
        result.add_suffix(&release);

        let error = detail_row("", "");
        error.add_css_class("error");
        error.set_accessible_role(gtk::AccessibleRole::Alert);
        error.set_visible(false);

        let warning = detail_row("", "");
        warning.add_css_class("warning");
        warning.set_accessible_role(gtk::AccessibleRole::Alert);
        warning.set_visible(false);

        Self {
            installed,
            check,
            check_intent,
            spinner,
            result,
            release,
            error,
            warning,
        }
    }

    fn attach(&self, group: &adw::PreferencesGroup) {
        group.add(&self.installed);
        group.add(&self.result);
        group.add(&self.error);
        group.add(&self.warning);
    }

    fn render(&self, presentation: &UpdateCheckPresentation) {
        self.installed
            .set_subtitle(presentation.installed_version_label());
        let action = presentation.check_action();
        self.check_intent.replace(Some(action.intent()));
        self.check.set_label(action.label());
        self.check.set_sensitive(action.enabled());
        self.check
            .update_property(&[gtk::accessible::Property::Label(action.label())]);
        self.spinner.set_visible(presentation.checking());
        self.spinner.set_spinning(presentation.checking());

        if let Some(report) = presentation.result() {
            self.result.set_visible(true);
            self.result.set_title(&report.title());
            self.result.set_subtitle(&report.description());
            if let Some(release) = report.available_release.as_ref() {
                self.release.set_uri(&release.url);
                self.release.set_visible(true);
            } else {
                self.release.set_visible(false);
            }
            if let Some(warning) = report.warning.as_deref() {
                self.warning.set_title("Cache warning");
                self.warning.set_subtitle(warning);
                self.warning.set_visible(true);
            } else {
                self.warning.set_visible(false);
            }
        } else {
            self.result.set_visible(false);
            self.release.set_visible(false);
            self.warning.set_visible(false);
        }

        if let Some(error) = presentation.error() {
            self.error.set_title(error.summary());
            self.error.set_subtitle(error.detail());
            self.error.set_visible(true);
        } else {
            self.error.set_visible(false);
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
                    .text(text)
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
        };
        // Populate values without producing commit intents.
        native.render_editor(initial.editor());
        native.render_state(initial, true);
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
                // Preserve a newer, unsubmitted draft and leave the caret alone on success.
                if entry.text().as_str() == finalized.borrow().as_str() {
                    if entry.text().as_str() != text {
                        entry.set_text(text);
                    }
                    finalized.replace(text.clone());
                }
            }
            _ => unreachable!("a behavior setting keeps its declared editor type"),
        }
        self.rendering.set(false);
    }

    fn render(&self, current: &SettingsRow, visible: bool) {
        let previous = self.presentation.borrow().clone();
        if previous == *current && self.row.is_visible() == visible {
            return;
        }
        if current.edit_status() != SettingsEditStatus::Saving
            && (previous.editor() != current.editor()
                || previous.edit_status() == SettingsEditStatus::Saving)
        {
            self.render_editor(current.editor());
        }
        self.render_state(current, visible);
    }

    fn render_state(&self, current: &SettingsRow, visible: bool) {
        self.rendering.set(true);
        self.row.set_visible(visible);
        self.row.set_sensitive(current.editor_enabled());
        self.row.update_property(&[
            gtk::accessible::Property::Label(current.title()),
            gtk::accessible::Property::Description(&format!(
                "{} {}",
                current.description(),
                current.value_label()
            )),
        ]);
        self.problem
            .set_visible(visible && current.problem().is_some());
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
        self.feedback.set_visible(visible && feedback.is_some());
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
        self.rendering.set(false);
    }
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
    let refresh = editing.handle_intent(SettingsIntent::Refresh).unwrap();
    view.render(refresh.presentation());
    assert_eq!(view.root.visible_child_name().as_deref(), Some("settings"));
    assert!(
        view.page.is_mapped(),
        "refresh must not unmap populated settings"
    );
    assert!(view.rows.borrow().iter().all(|row| row.row.is_sensitive()));
    // A new edit supersedes this refresh; its stale read must not replace the value.
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
    assert!(editing
        .complete_read(
            refresh.read_operation().unwrap(),
            Ok(presentation.groups().to_vec())
        )
        .is_none());
    view.render(writing.presentation());
    assert!(view.rows.borrow().iter().all(|row| row.row.is_sensitive()));
    assert!(!view.rows.borrow()[2].feedback.is_visible());
    assert_eq!(
        view.groups.borrow()[0]
            .measure(gtk::Orientation::Vertical, 600)
            .1,
        initial_height,
        "an ordinary setting change must not shift the layout"
    );
    assert!(
        GtkWindowExt::focus(&window).is_some_and(|focus| focus
            == entry.clone().upcast::<gtk::Widget>()
            || focus.is_ancestor(&entry)),
        "saving must keep keyboard focus"
    );
    assert_eq!(
        entry.text(),
        "900",
        "saving must preserve the submitted draft"
    );
    assert!(entry.is_sensitive());
    assert!(
        intents.borrow().is_empty(),
        "rendering must not resubmit the value"
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
    match &view.rows.borrow()[0].editor {
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

    let writing = editing
        .handle_intent(SettingsIntent::Commit {
            setting: BehaviorSetting::ScreenIdleTimeout,
            value: "720".into(),
        })
        .unwrap();
    view.render(writing.presentation());
    entry.grab_focus();
    entry.set_text("721");
    entry.set_position(1);
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
        "721",
        "completion must preserve a newer unsubmitted draft"
    );
    assert_eq!(entry.position(), 1, "completion must not move the caret");

    // An external settings refresh can hide the currently focused idle editor.
    // Keep its native widget/draft and hide its associated errors too.
    entry.grab_focus();
    intents.borrow_mut().clear();
    let disabled = SettingsPresentation::from_store(
        &ConfigEnvReader::parse(
            "/unused/config.env",
            "screen_idle_blank=disabled\nscreen_backend=invalid\nscreen_idle_timeout=600\n",
        )
        .into_store(),
    );
    view.render(&disabled);
    pump_until(|| !entry.is_mapped());
    assert!(rows[0].is_visible(), "Idle blanking remains available");
    assert!(!rows[1].is_visible(), "Desktop integration is hidden");
    assert!(!rows[2].is_visible(), "Idle timeout is hidden");
    assert!(rows[3].is_visible(), "Restore policy remains available");
    assert!(!view.rows.borrow()[1].problem.is_visible());
    assert!(!view.rows.borrow()[2].feedback.is_visible());
    assert!(
        intents.borrow().is_empty(),
        "hiding rows must not save a draft"
    );
    window.child_focus(gtk::DirectionType::TabForward);
    assert!(GtkWindowExt::focus(&window).is_some_and(|focus| focus.is_mapped()));

    view.render(failed.presentation());
    pump_until(|| entry.is_mapped());
    assert!(rows[1].is_visible());
    assert!(rows[2].is_visible());
    assert!(view.rows.borrow()[2].feedback.is_visible());
    assert_eq!(entry.text(), "721", "hiding must preserve the editor draft");
    assert!(view
        .rows
        .borrow()
        .iter()
        .zip(&rows)
        .all(|(native, row)| native.row == *row));

    window.set_default_size(360, 600);
    pump_until(|| window.width() <= 360);
    let (minimum, _, _, _) = view.widget().measure(gtk::Orientation::Horizontal, -1);
    assert!(
        minimum <= 360,
        "settings rows must fit a narrow window: {minimum}"
    );
    window.close();
    choice_row_click_opens_the_value_menu(application);
    update_check_renderer_scenarios(application);
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
    let choice = match &view.rows.borrow()[1].editor {
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
    let expected = ready.presentation().groups()[0].rows()[1]
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

#[cfg(test)]
fn update_check_renderer_scenarios(application: &adw::Application) {
    use crate::controller_test_support::pump_until;
    use lg_buddy::presentation::update_check::{AvailableUpdate, UpdateCheckReport};
    use lg_buddy::settings::{ConfigEnvReader, SettingsStore};
    use lg_buddy::settings_view::{SettingsApplication, SettingsIntent, UpdateCheckError};
    use lg_buddy::updates::UpdateChannel;

    let intents = Rc::new(RefCell::new(Vec::new()));
    let on_intent: Rc<dyn Fn(SettingsIntent)> = Rc::new({
        let intents = Rc::clone(&intents);
        move |intent| intents.borrow_mut().push(intent)
    });
    let view = SettingsView::new(on_intent);
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("LG Buddy Update Check Renderer Test")
        .default_width(900)
        .default_height(700)
        .content(view.widget())
        .build();
    let (mut model, opening) = SettingsApplication::open();
    let store = SettingsStore::from_reader(ConfigEnvReader::parse(
        "/unused/config.env",
        "updates_channel=stable\n",
    ));
    let ready = model
        .complete_read(
            opening.read_operation().unwrap(),
            Ok(SettingsPresentation::from_store(&store).groups().to_vec()),
        )
        .unwrap();
    view.render(ready.presentation());
    window.present();
    pump_until(|| view.page.is_mapped() && view.groups.borrow().len() == 3);

    let stable_rows: Vec<_> = view
        .rows
        .borrow()
        .iter()
        .map(|row| row.row.clone())
        .collect();
    assert_eq!(stable_rows.len(), 7);
    assert_eq!(
        view.update_check.installed.title().as_str(),
        "Installed version"
    );
    assert!(!view.update_check.spinner.is_visible());
    assert!(!view.update_check.result.is_visible());
    assert!(!view.update_check.error.is_visible());
    assert!(!view.update_check.warning.is_visible());

    let check = view.update_check.check.clone();
    assert_eq!(check.accessible_role(), gtk::AccessibleRole::Button);
    assert!(check.grab_focus());
    intents.borrow_mut().clear();
    check.emit_clicked();
    assert_eq!(
        *intents.borrow(),
        vec![SettingsIntent::CheckForUpdates],
        "only activating Check should submit an update-check intent"
    );
    let checking = model
        .handle_intent(intents.borrow_mut().pop().unwrap())
        .unwrap();
    let operation = checking.update_check_operation().unwrap();
    view.render(checking.presentation());
    assert!(view.update_check.spinner.is_visible());
    assert_eq!(check.label().as_deref(), Some("Checking…"));
    assert!(!check.is_sensitive());
    assert!(
        check.is_focusable(),
        "checking must leave the Check button available to the keyboard focus ring"
    );
    assert!(
        intents.borrow().is_empty(),
        "rendering must not submit an intent"
    );

    let available = UpdateCheckReport {
        installed_version: "1.6.0".into(),
        channel: UpdateChannel::Stable,
        available_release: Some(AvailableUpdate {
            version: "1.7.0".into(),
            url: "https://example.test/releases/v1.7.0?name=release%3C1%3E".into(),
        }),
        warning: Some("Cache <could not>& be refreshed.".into()),
    };
    let success = model
        .complete_update_check(operation, Ok(available))
        .unwrap();
    view.render(success.presentation());
    assert!(!view.update_check.spinner.is_visible());
    assert!(check.is_sensitive());
    assert_eq!(check.label().as_deref(), Some("Check for updates"));
    assert!(view.update_check.result.is_visible());
    assert_eq!(
        view.update_check.result.title().as_str(),
        "Update available: 1.7.0"
    );
    assert!(view
        .update_check
        .result
        .subtitle()
        .is_some_and(|text| text.contains("stable") && text.contains("1.6.0")));
    assert!(view.update_check.release.is_visible());
    assert_eq!(
        view.update_check.release.uri().as_str(),
        "https://example.test/releases/v1.7.0?name=release%3C1%3E"
    );
    assert_eq!(
        view.update_check.release.accessible_role(),
        gtk::AccessibleRole::Link
    );
    assert!(view.update_check.warning.is_visible());
    assert_eq!(
        view.update_check.warning.accessible_role(),
        gtk::AccessibleRole::Alert
    );
    assert!(view
        .update_check
        .warning
        .subtitle()
        .is_some_and(|text| text == "Cache <could not>& be refreshed."));
    assert!(
        intents.borrow().is_empty(),
        "rendering must not submit an intent"
    );

    let checking_again = model
        .handle_intent(SettingsIntent::CheckForUpdates)
        .unwrap();
    let operation = checking_again.update_check_operation().unwrap();
    view.render(checking_again.presentation());
    assert!(view.update_check.result.is_visible());
    assert!(view.update_check.warning.is_visible());
    let current = UpdateCheckReport {
        installed_version: "1.6.0".into(),
        channel: UpdateChannel::Stable,
        available_release: None,
        warning: None,
    };
    let current = model.complete_update_check(operation, Ok(current)).unwrap();
    view.render(current.presentation());
    assert!(view.update_check.result.is_visible());
    assert_eq!(
        view.update_check.result.title().as_str(),
        "No newer release available"
    );
    assert!(!view.update_check.release.is_visible());
    assert!(!view.update_check.warning.is_visible());

    let checking_again = model
        .handle_intent(SettingsIntent::CheckForUpdates)
        .unwrap();
    let operation = checking_again.update_check_operation().unwrap();
    view.render(checking_again.presentation());
    let failed = model
        .complete_update_check(operation, Err(UpdateCheckError::stopped()))
        .unwrap();
    view.render(failed.presentation());
    assert!(view.update_check.result.is_visible());
    assert_eq!(
        view.update_check.result.title().as_str(),
        "No newer release available"
    );
    assert!(view.update_check.error.is_visible());
    assert_eq!(
        view.update_check.error.accessible_role(),
        gtk::AccessibleRole::Alert
    );
    assert_eq!(check.label().as_deref(), Some("Retry check"));
    assert!(check.is_sensitive());

    intents.borrow_mut().clear();
    check.emit_clicked();
    assert_eq!(
        *intents.borrow(),
        vec![SettingsIntent::CheckForUpdates],
        "Retry check must reuse the application-owned intent"
    );
    intents.borrow_mut().clear();
    view.render(failed.presentation());
    assert!(
        intents.borrow().is_empty(),
        "rendering must not submit an intent"
    );
    assert!(view
        .rows
        .borrow()
        .iter()
        .zip(&stable_rows)
        .all(|(current, original)| current.row == *original));

    window.set_default_size(360, 700);
    pump_until(|| window.width() <= 360);
    let (minimum, _, _, _) = view.widget().measure(gtk::Orientation::Horizontal, -1);
    assert!(
        minimum <= 360,
        "update-check controls must fit a narrow window: {minimum}"
    );
    window.close();
}
