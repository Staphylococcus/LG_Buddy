use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use lg_buddy::presentation::settings::{
    SettingsCommitPolicy, SettingsEditStatus, SettingsEditor, SettingsFeedbackSeverity,
    SettingsPresentation, SettingsRow, SettingsStatus,
};
use lg_buddy::presentation::updater::UpdaterPresentation;
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
    updater: UpdaterView,
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
        let updater = UpdaterView::new(Rc::clone(&on_intent));
        Self {
            root,
            page,
            groups: RefCell::new(Vec::new()),
            rows: RefCell::new(Vec::new()),
            on_intent,
            status,
            retry,
            retry_intent,
            updater,
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
                        self.page.add(&native);
                        self.groups.borrow_mut().push(native);
                        if group.title() == "Updates" {
                            self.page.add(&self.updater.group);
                        }
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
        self.updater.render(&presentation.updater());
    }
}

/// One stable card renders the current step of the application-owned workflow.
struct UpdaterView {
    group: adw::PreferencesGroup,
    row: adw::ActionRow,
    action: gtk::Button,
    action_intent: Rc<RefCell<Option<SettingsIntent>>>,
    cancel: gtk::Button,
    cancel_intent: Rc<RefCell<Option<SettingsIntent>>>,
    actions: gtk::Box,
    footer: gtk::ListBoxRow,
    release: gtk::LinkButton,
    spinner: gtk::Spinner,
    warning: gtk::Label,
    details: adw::ExpanderRow,
    details_text: gtk::Label,
}

impl UpdaterView {
    fn new(on_intent: Rc<dyn Fn(SettingsIntent)>) -> Self {
        let group = adw::PreferencesGroup::new();
        let card = gtk::ListBox::new();
        card.set_selection_mode(gtk::SelectionMode::None);
        card.add_css_class("boxed-list");
        let row = detail_row("", "");
        row.set_title_lines(0);
        row.set_accessible_role(gtk::AccessibleRole::Status);
        let spinner = gtk::Spinner::new();
        spinner.set_valign(gtk::Align::Center);
        row.add_suffix(&spinner);
        card.append(&row);

        let warning = gtk::Label::builder()
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .xalign(0.0)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(12)
            .build();
        warning.add_css_class("warning");
        warning.set_accessible_role(gtk::AccessibleRole::Alert);
        let footer_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        footer_content.append(&warning);

        let actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::End)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(12)
            .build();
        let release = gtk::LinkButton::builder()
            .label("View release")
            .uri("https://github.com/Staphylococcus/LG_Buddy/releases")
            .build();
        release.update_property(&[gtk::accessible::Property::Label("View available release")]);
        actions.append(&release);
        let cancel = gtk::Button::new();
        cancel.add_css_class("flat");
        let cancel_intent = Rc::new(RefCell::new(None));
        cancel.connect_clicked({
            let intent = Rc::clone(&cancel_intent);
            let on_intent = Rc::clone(&on_intent);
            move |_| {
                let intent = intent.borrow().clone();
                if let Some(intent) = intent {
                    on_intent(intent);
                }
            }
        });
        actions.append(&cancel);
        let action = gtk::Button::new();
        action.add_css_class("suggested-action");
        let action_intent = Rc::new(RefCell::new(None));
        action.connect_clicked({
            let intent = Rc::clone(&action_intent);
            move |_| {
                let intent = intent.borrow().clone();
                if let Some(intent) = intent {
                    on_intent(intent);
                }
            }
        });
        actions.append(&action);
        footer_content.append(&actions);
        let footer = gtk::ListBoxRow::builder()
            .activatable(false)
            .selectable(false)
            .child(&footer_content)
            .build();
        card.append(&footer);

        let details = adw::ExpanderRow::builder()
            .use_markup(false)
            .visible(false)
            .build();
        let details_text = gtk::Label::builder()
            .selectable(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(0.0)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        details.add_row(&details_text);
        card.append(&details);
        group.add(&card);
        Self {
            group,
            row,
            action,
            action_intent,
            cancel,
            cancel_intent,
            actions,
            footer,
            release,
            spinner,
            warning,
            details,
            details_text,
        }
    }

    fn render(&self, presentation: &UpdaterPresentation) {
        let action_had_focus = self.action.has_focus();
        let cancel_had_focus = self.cancel.has_focus();
        let release_had_focus = self.release.has_focus();
        self.row.set_title(presentation.title());
        self.row.set_subtitle(presentation.description());
        self.row.update_property(&[
            gtk::accessible::Property::Label(presentation.title()),
            gtk::accessible::Property::Description(presentation.description()),
        ]);
        if presentation.is_error() {
            self.row.add_css_class("error");
        } else {
            self.row.remove_css_class("error");
        }
        for (button, intent, action) in [
            (&self.action, &self.action_intent, presentation.action()),
            (
                &self.cancel,
                &self.cancel_intent,
                presentation.cancel_action(),
            ),
        ] {
            button.set_visible(action.is_some());
            button.set_sensitive(action.is_some_and(|action| action.enabled()));
            button.set_label(action.map_or("", |action| action.label()));
            button.update_property(&[gtk::accessible::Property::Label(
                action.map_or("", |action| action.label()),
            )]);
            intent.replace(action.map(|action| action.intent()));
        }
        self.spinner.set_visible(presentation.busy());
        self.spinner.set_spinning(presentation.busy());
        self.spinner
            .update_property(&[gtk::accessible::Property::Label(presentation.title())]);
        self.release.set_visible(presentation.release().is_some());
        if let Some(release) = presentation.release() {
            self.release.set_uri(&release.url);
        }
        let has_actions = presentation.action().is_some()
            || presentation.cancel_action().is_some()
            || presentation.release().is_some();
        self.actions.set_visible(has_actions);
        self.warning.set_visible(presentation.warning().is_some());
        self.warning.set_text(presentation.warning().unwrap_or(""));
        self.footer
            .set_visible(has_actions || presentation.warning().is_some());

        if let Some(details) = presentation.details() {
            if self.details_text.text().as_str() != details {
                self.details.set_expanded(false);
                self.details_text.set_text(details);
            }
            self.details.set_title(presentation.details_title());
            self.details.set_visible(true);
        } else {
            self.details.set_expanded(false);
            self.details.set_visible(false);
            self.details_text.set_text("");
        }
        // Follow a disappearing action within this card, without taking focus
        // from another setting when an asynchronous completion arrives.
        if (action_had_focus && !self.action.is_visible())
            || (cancel_had_focus && !self.cancel.is_visible())
            || (release_had_focus && !self.release.is_visible())
        {
            if self.action.is_visible() && self.action.is_sensitive() {
                self.action.grab_focus();
            } else if self.cancel.is_visible() && self.cancel.is_sensitive() {
                self.cancel.grab_focus();
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
        let show_warning = current.problem().is_some() || is_warning;
        // An empty prefix box still reserves spacing in Adwaita. Attach the
        // icon only when needed so ordinary rows retain their native inset.
        if show_warning && self.warning.parent().is_none() {
            self.row.add_prefix(&self.warning);
        } else if !show_warning && self.warning.parent().is_some() {
            self.row.remove(&self.warning);
        }
        self.warning.set_visible(show_warning);
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
    assert!(!rows.iter().any(|row| row.is::<adw::ExpanderRow>()));
    assert!(!view.updater.details.is_visible());
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
    updater_renderer_scenarios(application);
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
fn updater_renderer_scenarios(application: &adw::Application) {
    use crate::controller_test_support::pump_until;
    use lg_buddy::presentation::update_check::{AvailableUpdate, UpdateCheckReport};
    use lg_buddy::settings::{ConfigEnvReader, SettingsStore};
    use lg_buddy::settings_view::{BehaviorSetting, SettingsApplication, UpdateCheckError};
    use lg_buddy::update_flow::UpdateInstallOutcome;
    use lg_buddy::update_install::{
        InstalledUpdate, PreparedUpdateInstall, UpdateInstallError, UpdateInstallStage,
    };
    use lg_buddy::updates::UpdateChannel;
    use lg_buddy::version::VersionInfo;

    fn title_x(row: &adw::ActionRow, root: &impl IsA<gtk::Widget>) -> f32 {
        fn find(widget: &gtk::Widget, title: &str) -> Option<gtk::Label> {
            if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                if label.text() == title {
                    return Some(label.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(label) = find(&current, title) {
                    return Some(label);
                }
                child = current.next_sibling();
            }
            None
        }
        find(row.upcast_ref(), &row.title())
            .unwrap()
            .compute_bounds(root)
            .unwrap()
            .x()
    }
    fn prepared() -> PreparedUpdateInstall {
        PreparedUpdateInstall::from_parts(
            VersionInfo::current(),
            "1.7.0".parse().unwrap(),
            UpdateChannel::Stable,
            "https://example.test/releases/v1.7.0",
            "v1.7.0",
            "x86_64-unknown-linux-gnu",
            "newer-commit",
        )
    }
    fn assert_narrow(view: &SettingsView) {
        let (minimum, _, _, _) = view.widget().measure(gtk::Orientation::Horizontal, -1);
        assert!(
            minimum <= 360,
            "updater card must fit a narrow window: {minimum}"
        );
    }

    let intents = Rc::new(RefCell::new(Vec::new()));
    let view = SettingsView::new(Rc::new({
        let intents = Rc::clone(&intents);
        move |intent| intents.borrow_mut().push(intent)
    }));
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("LG Buddy Updater Renderer Test")
        .default_width(900)
        .default_height(700)
        .content(view.widget())
        .build();
    let (mut model, opening) = SettingsApplication::open();
    let store = SettingsStore::from_reader(ConfigEnvReader::parse(
        "/unused/config.env",
        "updates_channel=stable\n",
    ));
    model
        .complete_read(
            opening.read_operation().unwrap(),
            Ok(SettingsPresentation::from_store(&store).groups().to_vec()),
        )
        .unwrap();
    view.render(model.presentation());
    window.present();
    pump_until(|| view.updater.row.is_mapped() && view.updater.row.width() > 0);
    let stable_rows: Vec<_> = view
        .rows
        .borrow()
        .iter()
        .map(|row| row.row.clone())
        .collect();
    let card_row = view.updater.row.clone();
    let action = view.updater.action.clone();
    assert_eq!(stable_rows.len(), 7);
    assert_eq!(card_row.title().as_str(), "Installed version");
    assert_eq!(action.label().as_deref(), Some("Check for updates"));
    assert!(!view.updater.spinner.is_visible());
    assert!(!view.updater.details.is_visible());
    assert!(!view.updater.release.is_visible());
    assert!(!view.updater.warning.is_visible());
    assert_eq!(card_row.accessible_role(), gtk::AccessibleRole::Status);

    // Compare native label allocations, not hard-coded padding or widget types.
    let rows = view.rows.borrow();
    for row in rows.iter().filter(|row| {
        matches!(
            row.presentation.borrow().setting(),
            BehaviorSetting::UpdatesAutoCheck | BehaviorSetting::UpdatesChannel
        )
    }) {
        assert!(row.warning.parent().is_none());
        assert!(
            (title_x(&row.row, &window) - title_x(&card_row, &window)).abs() < 1.0,
            "updater and normal settings text must share their left edge: setting={}, updater={}",
            title_x(&row.row, &window),
            title_x(&card_row, &window)
        );
    }
    drop(rows);

    assert!(action.grab_focus());
    action.emit_clicked();
    assert_eq!(*intents.borrow(), vec![SettingsIntent::CheckForUpdates]);
    intents.borrow_mut().clear();
    let checking = model
        .handle_intent(SettingsIntent::CheckForUpdates)
        .unwrap();
    view.render(checking.presentation());
    assert!(view.updater.spinner.is_visible());
    assert!(!action.is_sensitive());
    assert_eq!(action.label().as_deref(), Some("Checking…"));
    assert!(action.is_focusable());
    assert!(intents.borrow().is_empty());

    let report = UpdateCheckReport {
        installed_version: "1.6.0".into(),
        channel: UpdateChannel::Stable,
        available_release: Some(AvailableUpdate {
            version: "1.7.0".into(),
            url: "https://example.test/releases/v1.7.0?name=release%3C1%3E".into(),
        }),
        warning: Some("Cache <could not>& be refreshed.".into()),
    };
    model
        .complete_update_check(checking.update_check_operation().unwrap(), Ok(report))
        .unwrap();
    view.render(model.presentation());
    assert_eq!(card_row.title().as_str(), "Update available: 1.7.0");
    assert_eq!(action.label().as_deref(), Some("Install update…"));
    assert!(view.updater.release.is_visible());
    assert_eq!(
        view.updater.release.uri().as_str(),
        "https://example.test/releases/v1.7.0?name=release%3C1%3E"
    );
    assert_eq!(
        view.updater.warning.text().as_str(),
        "Cache <could not>& be refreshed."
    );
    assert!(view.updater.warning.is_visible());
    assert!(!view.updater.spinner.is_visible());
    assert_narrow(&view);

    assert!(action.grab_focus());
    action.emit_clicked();
    assert_eq!(
        *intents.borrow(),
        vec![SettingsIntent::PrepareUpdateInstall]
    );
    intents.borrow_mut().clear();
    model
        .handle_intent(SettingsIntent::PrepareUpdateInstall)
        .unwrap();
    view.render(model.presentation());
    assert_eq!(card_row.title().as_str(), "Preparing update…");
    assert!(!action.is_visible());
    assert!(view.updater.cancel.is_visible());
    assert!(view.updater.cancel.has_focus());
    assert!(!view.updater.release.is_visible());
    assert!(!view.updater.warning.is_visible());
    view.updater.cancel.emit_clicked();
    assert_eq!(*intents.borrow(), vec![SettingsIntent::CancelUpdateInstall]);
    intents.borrow_mut().clear();
    model
        .handle_intent(SettingsIntent::CancelUpdateInstall)
        .unwrap();
    view.render(model.presentation());
    assert_eq!(card_row.title().as_str(), "Update available: 1.7.0");
    assert!(action.has_focus());

    let preparing = model
        .handle_intent(SettingsIntent::PrepareUpdateInstall)
        .unwrap();
    model
        .complete_update_install(
            preparing.update_install_operation().unwrap(),
            Ok(UpdateInstallOutcome::Prepared(prepared())),
        )
        .unwrap();
    view.render(model.presentation());
    assert_eq!(card_row.title().as_str(), "Install LG Buddy 1.7.0?");
    assert_eq!(action.label().as_deref(), Some("Install and restart"));
    assert!(view.updater.cancel.is_visible());
    assert!(!view.updater.release.is_visible());
    assert!(!view.updater.spinner.is_visible());
    assert_narrow(&view);
    action.emit_clicked();
    assert_eq!(
        *intents.borrow(),
        vec![SettingsIntent::ConfirmUpdateInstall]
    );
    intents.borrow_mut().clear();
    let installing = model
        .handle_intent(SettingsIntent::ConfirmUpdateInstall)
        .unwrap();
    let operation = installing.update_install_operation().unwrap();
    model
        .update_install_progress(operation, UpdateInstallStage::Acquiring)
        .unwrap();
    view.render(model.presentation());
    assert_eq!(
        card_row.title().as_str(),
        "Downloading and verifying update…"
    );
    assert!(view.updater.spinner.is_visible());
    assert!(view.updater.cancel.is_visible());
    assert!(!action.is_visible());
    model
        .update_install_progress(operation, UpdateInstallStage::Installing)
        .unwrap();
    view.render(model.presentation());
    assert_eq!(card_row.title().as_str(), "Installing update…");
    assert!(!view.updater.actions.is_visible());
    assert!(!view.updater.cancel.is_visible());

    model
        .complete_update_install(
            operation,
            Err(UpdateInstallError::InstallerFailedWithOutput {
                code: Some(1),
                output: "install: No space left on device\naccess_token=private-value".into(),
                mutation_started: true,
            }
            .into()),
        )
        .unwrap();
    view.render(model.presentation());
    assert_eq!(card_row.title().as_str(), "Could not install update");
    assert!(card_row.has_css_class("error"));
    assert_eq!(action.label().as_deref(), Some("Retry update"));
    assert!(!view.updater.release.is_visible());
    assert!(view.updater.details.is_visible());
    assert!(!view.updater.details.is_expanded());
    assert!(view.updater.details_text.is_selectable());
    assert!(view
        .updater
        .details_text
        .text()
        .contains("No space left on device"));
    assert!(!view.updater.details_text.text().contains("private-value"));
    view.updater.details.set_expanded(true);
    view.render(model.presentation());
    assert!(view.updater.details.is_expanded());
    assert!(view.updater.details_text.grab_focus());
    assert_narrow(&view);
    action.emit_clicked();
    assert_eq!(
        *intents.borrow(),
        vec![SettingsIntent::PrepareUpdateInstall]
    );
    intents.borrow_mut().clear();
    let preparing = model
        .handle_intent(SettingsIntent::PrepareUpdateInstall)
        .unwrap();
    view.render(model.presentation());
    assert_eq!(card_row.title().as_str(), "Preparing update…");
    assert!(!card_row.has_css_class("error"));
    assert_eq!(view.updater.details.title().as_str(), "Last update failure");
    assert!(
        view.updater.details_text.has_focus(),
        "progress must not steal diagnostic selection focus"
    );
    model
        .complete_update_install(
            preparing.update_install_operation().unwrap(),
            Ok(UpdateInstallOutcome::Prepared(prepared())),
        )
        .unwrap();
    let installing = model
        .handle_intent(SettingsIntent::ConfirmUpdateInstall)
        .unwrap();
    let installed = InstalledUpdate::from_parts(
        "1.7.0".parse().unwrap(),
        UpdateChannel::Stable,
        "v1.7.0",
        "x86_64-unknown-linux-gnu",
        "newer-commit",
        "/unused/lg-buddy",
        "/unused/lg-buddy-gui",
    );
    let restarting = model
        .complete_update_install(
            installing.update_install_operation().unwrap(),
            Ok(UpdateInstallOutcome::Installed(installed)),
        )
        .unwrap();
    view.render(model.presentation());
    assert_eq!(card_row.title().as_str(), "Restarting LG Buddy…");
    assert!(!view.updater.actions.is_visible());
    assert!(view.updater.spinner.is_visible());
    model
        .complete_update_install(
            restarting.update_install_operation().unwrap(),
            Err(lg_buddy::update_flow::UpdateInstallFailure::stopped()),
        )
        .unwrap();
    view.render(model.presentation());
    assert_eq!(action.label().as_deref(), Some("Retry restart"));
    action.emit_clicked();
    assert_eq!(
        *intents.borrow(),
        vec![SettingsIntent::RelaunchUpdatedApplication]
    );
    intents.borrow_mut().clear();

    // A check replaces the prior result or failure in the same card.
    let checking = model
        .handle_intent(SettingsIntent::CheckForUpdates)
        .unwrap();
    view.render(checking.presentation());
    assert!(view.updater.spinner.is_visible());
    assert!(!card_row.has_css_class("error"));
    assert!(!view.updater.release.is_visible());
    model
        .complete_update_check(
            checking.update_check_operation().unwrap(),
            Err(UpdateCheckError::stopped()),
        )
        .unwrap();
    view.render(model.presentation());
    assert_eq!(card_row.title().as_str(), "Update check stopped");
    assert_eq!(action.label().as_deref(), Some("Retry check"));
    assert!(card_row.has_css_class("error"));
    action.emit_clicked();
    assert_eq!(*intents.borrow(), vec![SettingsIntent::CheckForUpdates]);
    intents.borrow_mut().clear();
    view.render(model.presentation());
    assert!(
        intents.borrow().is_empty(),
        "rendering must not submit intents"
    );
    assert_eq!(view.updater.row, card_row);
    assert_eq!(view.updater.action, action);
    assert!(view
        .rows
        .borrow()
        .iter()
        .zip(&stable_rows)
        .all(|(current, original)| current.row == *original));
    window.set_default_size(360, 700);
    pump_until(|| window.width() <= 360);
    assert_narrow(&view);
    window.close();
}
