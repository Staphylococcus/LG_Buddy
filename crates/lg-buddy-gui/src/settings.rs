use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use lg_buddy::presentation::settings::{
    SettingsCommitPolicy, SettingsEditStatus, SettingsEditor, SettingsFeedbackSeverity,
    SettingsPresentation, SettingsRow, SettingsStatus,
};
use lg_buddy::settings_view::SettingsIntent;
use lg_buddy::settings_view::UpdateNotice;

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
                        if group.title() == "Updates" {
                            native.add(&self.updater.row);
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

    pub(crate) fn show_update_notice(&self, notice: &UpdateNotice, overlay: &adw::ToastOverlay) {
        self.updater.show_notice(notice, overlay);
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
        self.updater.render(presentation);
    }
}

/// One native settings row launches the temporary installation dialog.
struct UpdaterView {
    row: adw::ActionRow,
    action: gtk::Button,
    action_intent: Rc<RefCell<Option<SettingsIntent>>>,
    dialog: adw::Dialog,
    title: gtk::Label,
    description: gtk::Label,
    progress: gtk::ProgressBar,
    install: gtk::Button,
    actions: gtk::Box,
    install_intent: Rc<RefCell<Option<SettingsIntent>>>,
    cancel: gtk::Button,
    cancel_intent: Rc<RefCell<Option<SettingsIntent>>>,
    release: gtk::LinkButton,
    presented: Cell<bool>,
    close_pending: Rc<Cell<bool>>,
    focus_after_close: Rc<Cell<bool>>,
    pulse: RefCell<Option<gtk::glib::SourceId>>,
    notice: RefCell<Option<adw::Toast>>,
}

impl UpdaterView {
    fn new(on_intent: Rc<dyn Fn(SettingsIntent)>) -> Self {
        let row = detail_row("", "");
        row.set_title_lines(0);
        let (action, action_intent) = updater_button(Rc::clone(&on_intent));
        action.set_valign(gtk::Align::Center);
        row.add_suffix(&action);
        row.set_activatable_widget(Some(&action));

        let title = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .build();
        title.add_css_class("title-2");
        title.set_accessible_role(gtk::AccessibleRole::Status);
        let description = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .build();
        description.add_css_class("dim-label");
        let progress = gtk::ProgressBar::new();
        progress.set_pulse_step(0.08);
        let release = gtk::LinkButton::with_label(
            "https://github.com/Staphylococcus/LG_Buddy/releases",
            "View release",
        );
        release.set_halign(gtk::Align::Start);
        let (install, install_intent) = updater_button(Rc::clone(&on_intent));
        install.add_css_class("suggested-action");
        let (cancel, cancel_intent) = updater_button(Rc::clone(&on_intent));
        let actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::End)
            .margin_start(24)
            .margin_end(24)
            .margin_top(12)
            .margin_bottom(24)
            .build();
        actions.append(&cancel);
        actions.append(&install);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(24)
            .margin_end(24)
            .margin_top(12)
            .margin_bottom(24)
            .build();
        for widget in [
            title.upcast_ref::<gtk::Widget>(),
            description.upcast_ref(),
            progress.upcast_ref(),
            release.upcast_ref(),
        ] {
            content.append(widget);
        }
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&content)
            .build();
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        let toolbar = adw::ToolbarView::builder().content(&scroller).build();
        toolbar.add_top_bar(&header);
        toolbar.add_bottom_bar(&actions);
        let dialog = adw::Dialog::builder()
            .title("Software update")
            .content_width(440)
            .content_height(360)
            .child(&toolbar)
            // Ask the application to cancel; the installer can cross its
            // non-cancellable boundary before the next progress event arrives.
            .can_close(false)
            .build();
        let focus_after_close = Rc::new(Cell::new(false));
        dialog.connect_close_attempt({
            let cancel_intent = Rc::clone(&cancel_intent);
            move |_| {
                let intent = cancel_intent.borrow().clone();
                if let Some(intent) = intent {
                    on_intent(intent);
                }
            }
        });
        dialog.connect_closed({
            let action = action.downgrade();
            let focus_after_close = Rc::clone(&focus_after_close);
            move |_| {
                if let Some(action) = action.upgrade() {
                    if action.is_sensitive() {
                        focus_after_close.set(false);
                        action.grab_focus();
                    }
                }
            }
        });
        let close_pending = Rc::new(Cell::new(false));
        toolbar.connect_map({
            let close_pending = Rc::clone(&close_pending);
            let dialog = dialog.downgrade();
            let action = action.downgrade();
            move |_| {
                if !close_pending.get() {
                    return;
                }
                // libadwaita 1.5 ignores force_close before its first opening
                // frame. Finish that close after the content has mapped and
                // the opening callback has returned.
                let close_pending = Rc::clone(&close_pending);
                let dialog = dialog.clone();
                let action = action.clone();
                gtk::glib::idle_add_local_once(move || {
                    if close_pending.replace(false) {
                        if let Some(dialog) = dialog.upgrade() {
                            dialog.force_close();
                        }
                        if let Some(action) = action.upgrade() {
                            action.grab_focus();
                        }
                    }
                });
            }
        });
        Self {
            row,
            action,
            action_intent,
            dialog,
            title,
            description,
            progress,
            install,
            actions,
            install_intent,
            cancel,
            cancel_intent,
            release,
            presented: Cell::new(false),
            close_pending,
            focus_after_close,
            pulse: RefCell::new(None),
            notice: RefCell::new(None),
        }
    }

    fn render(&self, settings: &SettingsPresentation) {
        let row = settings.updater();
        self.row.set_title(row.title());
        self.row.set_subtitle(row.description());
        render_updater_button(&self.action, &self.action_intent, Some(row.action()));

        let install = settings.update_install();
        let active = install.busy() || install.cancel_action().is_some();
        if active || settings.update_check().checking() {
            if let Some(previous) = self.notice.take() {
                previous.dismiss();
            }
        }
        self.progress.set_visible(install.busy());
        // ponytail: the installer reports stages, so progress stays indeterminate.
        if install.busy() && self.pulse.borrow().is_none() {
            self.progress.pulse();
            let progress = self.progress.downgrade();
            self.pulse.replace(Some(gtk::glib::timeout_add_local(
                std::time::Duration::from_millis(100),
                move || {
                    let Some(progress) = progress.upgrade() else {
                        return gtk::glib::ControlFlow::Break;
                    };
                    progress.pulse();
                    gtk::glib::ControlFlow::Continue
                },
            )));
        } else if !install.busy() {
            if let Some(pulse) = self.pulse.take() {
                pulse.remove();
            }
            self.progress.set_fraction(0.0);
        }
        if !active {
            if self.presented.replace(false) {
                self.focus_after_close.set(true);
                if self.dialog.child().is_some_and(|child| child.is_mapped()) {
                    self.dialog.force_close();
                    let action = self.action.downgrade();
                    gtk::glib::idle_add_local_once(move || {
                        if let Some(action) = action.upgrade() {
                            action.grab_focus();
                        }
                    });
                } else {
                    self.close_pending.set(true);
                }
            }
            if self.focus_after_close.get()
                && self.action.is_sensitive()
                && self.dialog.parent().is_none()
            {
                self.focus_after_close.set(false);
                self.action.grab_focus();
            }
            return;
        }
        self.close_pending.set(false);
        let title = install.title().unwrap_or("Updating LG Buddy…");
        self.title.set_text(title);
        self.description.set_text(install.description());
        self.progress
            .update_property(&[gtk::accessible::Property::Label(title)]);
        render_updater_button(&self.install, &self.install_intent, install.action());
        render_updater_button(&self.cancel, &self.cancel_intent, install.cancel_action());
        self.actions
            .set_visible(install.action().is_some() || install.cancel_action().is_some());
        let release = install.release_url();
        self.release
            .set_visible(!install.busy() && release.is_some());
        if let Some(release) = release {
            self.release.set_uri(release);
        }
        if !self.presented.get() {
            if let Some(parent) = self.row.root().and_downcast::<gtk::Window>() {
                self.presented.set(true);
                self.dialog.present(Some(&parent));
                self.cancel.grab_focus();
            }
        }
    }

    fn show_notice(&self, notice: &UpdateNotice, overlay: &adw::ToastOverlay) {
        if let Some(previous) = self.notice.take() {
            previous.dismiss();
        }
        let toast = adw::Toast::builder()
            .title(notice.title())
            .use_markup(false)
            .build();
        if let Some(details) = notice.details() {
            toast.set_button_label(Some("Copy details"));
            toast.set_timeout(0);
            toast.set_priority(adw::ToastPriority::High);
            let details = details.to_owned();
            let clipboard = self.row.clipboard();
            toast.connect_button_clicked(move |_| clipboard.set_text(&details));
        }
        self.notice.replace(Some(toast.clone()));
        overlay.add_toast(toast);
    }
}

impl Drop for UpdaterView {
    fn drop(&mut self) {
        if let Some(pulse) = self.pulse.take() {
            pulse.remove();
        }
    }
}

fn updater_button(
    on_intent: Rc<dyn Fn(SettingsIntent)>,
) -> (gtk::Button, Rc<RefCell<Option<SettingsIntent>>>) {
    let button = gtk::Button::new();
    let intent = Rc::new(RefCell::new(None));
    button.connect_clicked({
        let intent = Rc::clone(&intent);
        move |_| {
            let intent = intent.borrow().clone();
            if let Some(intent) = intent {
                on_intent(intent);
            }
        }
    });
    (button, intent)
}

fn render_updater_button(
    button: &gtk::Button,
    intent: &RefCell<Option<SettingsIntent>>,
    action: Option<&lg_buddy::presentation::settings::SettingsAction>,
) {
    button.set_visible(action.is_some());
    button.set_sensitive(action.is_some_and(|action| action.enabled()));
    button.set_label(action.map_or("", |action| action.label()));
    intent.replace(action.map(|action| action.intent()));
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
    assert!(!view.updater.presented.get());
    assert_eq!(view.groups.borrow().len(), 3);
    assert_eq!(rows.len(), 8);
    assert!(
        descendants(rows[1].upcast_ref())
            .iter()
            .any(|widget| widget.accessible_role() == gtk::AccessibleRole::Switch),
        "idle inhibitor row contains an accessible toggle"
    );
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
        4
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
    let entry = match &view.rows.borrow()[3].editor {
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
    assert!(!view.rows.borrow()[3].feedback.is_visible());
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
    assert!(view.rows.borrow()[3].feedback.is_visible());
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
    match &view.rows.borrow()[1].editor {
        NativeEditor::Toggle(switch) => switch.set_active(true),
        _ => unreachable!(),
    }
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(SettingsIntent::SetEnabled {
            setting: BehaviorSetting::ScreenHonorIdleInhibitors,
            enabled: true,
        })
    );
    match &view.rows.borrow()[7].editor {
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
    let swayidle = SettingsPresentation::from_store(
        &ConfigEnvReader::parse(
            "/unused/config.env",
            "screen_idle_blank=enabled\nscreen_backend=swayidle\n",
        )
        .into_store(),
    );
    view.render(&swayidle);
    assert!(
        !rows[1].is_visible(),
        "native inhibitor setting is hidden for swayidle"
    );
    assert!(
        rows[2].is_visible(),
        "the selected swayidle backend remains visible"
    );
    assert!(
        rows[3].is_visible(),
        "idle timeout remains visible for swayidle"
    );
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
    assert!(!rows[1].is_visible(), "Idle inhibitor preference is hidden");
    assert!(!rows[2].is_visible(), "Desktop integration is hidden");
    assert!(!rows[3].is_visible(), "Idle timeout is hidden");
    assert!(rows[4].is_visible(), "Restore policy remains available");
    assert!(!view.rows.borrow()[2].problem.is_visible());
    assert!(!view.rows.borrow()[3].feedback.is_visible());
    assert!(
        intents.borrow().is_empty(),
        "hiding rows must not save a draft"
    );
    window.child_focus(gtk::DirectionType::TabForward);
    assert!(GtkWindowExt::focus(&window).is_some_and(|focus| focus.is_mapped()));

    view.render(failed.presentation());
    pump_until(|| entry.is_mapped());
    assert!(rows[2].is_visible());
    assert!(rows[3].is_visible());
    assert!(view.rows.borrow()[3].feedback.is_visible());
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
    let choice = match &view.rows.borrow()[2].editor {
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
    let expected = ready.presentation().groups()[0].rows()[2]
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
    use lg_buddy::presentation::update_check::UpdateCheckReport;
    use lg_buddy::settings::ConfigEnvReader;
    use lg_buddy::settings_view::{BehaviorSetting, SettingsApplication};
    use lg_buddy::update_flow::{UpdateInstallOutcome, UpdateInstallTask};
    use lg_buddy::update_install::{PreparedUpdateInstall, UpdateInstallError, UpdateInstallStage};
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
    let intents = Rc::new(RefCell::new(Vec::new()));
    let view = SettingsView::new(Rc::new({
        let intents = Rc::clone(&intents);
        move |intent| intents.borrow_mut().push(intent)
    }));
    let overlay = adw::ToastOverlay::new();
    overlay.set_child(Some(view.widget()));
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("LG Buddy Updater Renderer Test")
        .default_width(900)
        .default_height(700)
        .content(&overlay)
        .build();
    let (mut model, opening) = SettingsApplication::open();
    let store =
        ConfigEnvReader::parse("/unused/config.env", "updates_channel=stable\n").into_store();
    model
        .complete_read(
            opening.read_operation().unwrap(),
            Ok(SettingsPresentation::from_store(&store).groups().to_vec()),
        )
        .unwrap();
    view.render(model.presentation());
    window.present();
    pump_until(|| view.updater.row.is_mapped() && view.updater.row.width() > 0);
    let row = view.updater.row.clone();
    let action = view.updater.action.clone();
    assert_eq!(row.title(), "Installed version");
    assert_eq!(action.label().as_deref(), Some("Check for updates"));
    assert!(!view.updater.presented.get());
    assert!(view.updater.pulse.borrow().is_none());
    assert_eq!(action.accessible_role(), gtk::AccessibleRole::Button);
    for setting in view.rows.borrow().iter().filter(|row| {
        matches!(
            row.presentation.borrow().setting(),
            BehaviorSetting::UpdatesAutoCheck | BehaviorSetting::UpdatesChannel
        )
    }) {
        assert!(setting.warning.parent().is_none());
        assert!((title_x(&setting.row, &window) - title_x(&row, &window)).abs() < 1.0);
    }
    let bounds = action.compute_bounds(&row).unwrap();
    assert!(
        bounds.y() >= 0.0 && bounds.y() + bounds.height() <= row.height() as f32,
        "the action must fit inside the same native row"
    );

    action.emit_clicked();
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(SettingsIntent::CheckForUpdates)
    );
    let checking = model
        .handle_intent(SettingsIntent::CheckForUpdates)
        .unwrap();
    view.render(checking.presentation());
    assert!(!action.is_sensitive());
    assert_eq!(action.label().as_deref(), Some("Checking…"));
    assert!(!view.updater.presented.get());
    let current = model
        .complete_update_check(
            checking.update_check_operation().unwrap(),
            Ok(UpdateCheckReport {
                installed_version: "1.6.0".into(),
                channel: UpdateChannel::Stable,
                update_available: false,
                warning: None,
            }),
        )
        .unwrap();
    view.render(current.presentation());
    view.show_update_notice(current.update_notice().unwrap(), &overlay);
    assert_eq!(action.label().as_deref(), Some("Check for updates"));
    assert_eq!(row.title(), "Installed version");
    let current_toast = view.updater.notice.borrow().as_ref().unwrap().clone();
    assert_eq!(current_toast.title().as_deref(), Some("Already up to date"));
    view.render(model.presentation());
    assert_eq!(view.updater.notice.borrow().as_ref(), Some(&current_toast));

    let checking = model
        .handle_intent(SettingsIntent::CheckForUpdates)
        .unwrap();
    view.render(checking.presentation());
    assert!(view.updater.notice.borrow().is_none());
    model
        .complete_update_check(
            checking.update_check_operation().unwrap(),
            Ok(UpdateCheckReport {
                installed_version: "1.6.0".into(),
                channel: UpdateChannel::Stable,
                update_available: true,
                warning: None,
            }),
        )
        .unwrap();
    view.render(model.presentation());
    assert_eq!(row.title(), "Update available");
    assert_eq!(action.label().as_deref(), Some("Install update…"));
    action.emit_clicked();
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(SettingsIntent::PrepareUpdateInstall)
    );
    let preparation = model
        .handle_intent(SettingsIntent::PrepareUpdateInstall)
        .unwrap();
    view.render(preparation.presentation());
    // Complete before pumping GTK: on libadwaita 1.5 the dialog's opening
    // frame has not run yet, but the finished workflow must still dismiss it.
    let current = model
        .complete_update_install(
            preparation.update_install_operation().unwrap(),
            Ok(UpdateInstallOutcome::UpToDate),
        )
        .unwrap();
    view.render(current.presentation());
    view.show_update_notice(current.update_notice().unwrap(), &overlay);
    pump_until(|| window.visible_dialog().is_none() && view.updater.dialog.parent().is_none());
    assert!(action.has_focus());
    assert_eq!(action.label().as_deref(), Some("Check for updates"));

    let checking = model
        .handle_intent(SettingsIntent::CheckForUpdates)
        .unwrap();
    model
        .complete_update_check(
            checking.update_check_operation().unwrap(),
            Ok(UpdateCheckReport {
                installed_version: "1.6.0".into(),
                channel: UpdateChannel::Stable,
                update_available: true,
                warning: None,
            }),
        )
        .unwrap();
    let preparation = model
        .handle_intent(SettingsIntent::PrepareUpdateInstall)
        .unwrap();
    let preparation_operation = preparation.update_install_operation().unwrap().clone();
    view.render(preparation.presentation());
    pump_until(|| {
        window.visible_dialog().as_ref() == Some(&view.updater.dialog)
            && view.updater.cancel.is_mapped()
            && view.updater.cancel.height() > 0
    });
    assert!(view.updater.progress.is_visible());
    assert!(view.updater.pulse.borrow().is_some());
    assert!(!action.is_sensitive());
    assert_eq!(action.label().as_deref(), Some("Install update…"));
    // Closing requests cancellation through the application; it cannot bypass
    // a worker that already claimed the installer boundary.
    view.updater.dialog.close();
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(SettingsIntent::CancelUpdateInstall)
    );
    model
        .handle_intent(SettingsIntent::CancelUpdateInstall)
        .unwrap();
    view.render(model.presentation());
    pump_until(|| window.visible_dialog().is_none() && view.updater.dialog.parent().is_none());
    assert!(view.updater.pulse.borrow().is_none());

    // Cancellation closes the modal immediately, but the worker remains
    // active until its cleanup completes; focus returns when the action is
    // available again.
    assert!(!action.is_sensitive());
    model
        .complete_update_install(
            &preparation_operation,
            Err(UpdateInstallError::Cancelled.into()),
        )
        .unwrap();
    view.render(model.presentation());
    pump_until(|| action.is_sensitive());
    assert!(action.has_focus());

    let preparation = model
        .handle_intent(SettingsIntent::PrepareUpdateInstall)
        .unwrap();
    view.render(preparation.presentation());
    pump_until(|| view.updater.dialog.child().unwrap().is_mapped());
    model
        .complete_update_install(
            preparation.update_install_operation().unwrap(),
            Ok(UpdateInstallOutcome::Prepared(prepared())),
        )
        .unwrap();
    view.render(model.presentation());
    pump_until(|| view.updater.dialog.child().unwrap().is_mapped());
    assert_eq!(view.updater.title.text(), "Install LG Buddy 1.7.0?");
    assert!(!view.updater.progress.is_visible());
    assert!(view.updater.release.is_visible());
    assert_eq!(view.updater.release.uri(), prepared().release().url());
    assert_eq!(
        view.updater.install.label().as_deref(),
        Some("Install and restart")
    );
    assert!(view.updater.cancel.is_visible());
    pump_until(|| {
        if !view.updater.install.is_mapped() || view.updater.install.height() == 0 {
            return false;
        }
        let bounds = view
            .updater
            .install
            .compute_bounds(&view.updater.dialog)
            .unwrap();
        bounds.y() >= 0.0 && bounds.y() + bounds.height() <= view.updater.dialog.height() as f32
    });
    assert!(
        view.updater.install.is_mapped(),
        "confirmation must remain reachable without scrolling"
    );
    view.updater.install.emit_clicked();
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(SettingsIntent::ConfirmUpdateInstall)
    );
    let installing = model
        .handle_intent(SettingsIntent::ConfirmUpdateInstall)
        .unwrap();
    let operation = installing.update_install_operation().unwrap();
    view.render(installing.presentation());
    assert!(view.updater.progress.is_visible());
    assert!(!view.updater.install.is_visible());
    assert!(!view.updater.release.is_visible());
    if let UpdateInstallTask::Install { cancellation, .. } = operation.task() {
        cancellation.claim_installer_boundary().unwrap();
    } else {
        panic!("expected install task");
    }
    view.updater.dialog.close();
    assert_eq!(
        intents.borrow_mut().pop(),
        Some(SettingsIntent::CancelUpdateInstall)
    );
    assert!(model
        .handle_intent(SettingsIntent::CancelUpdateInstall)
        .is_none());
    assert!(view.updater.dialog.is_mapped());
    model
        .update_install_progress(operation, UpdateInstallStage::Installing)
        .unwrap();
    view.render(model.presentation());
    assert!(!view.updater.cancel.is_visible());
    assert_eq!(view.updater.title.text(), "Installing update…");
    view.updater.dialog.close();
    assert!(intents.borrow().is_empty());

    let failed = model
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
    view.render(failed.presentation());
    view.show_update_notice(failed.update_notice().unwrap(), &overlay);
    pump_until(|| window.visible_dialog().is_none());
    assert!(!view.updater.presented.get());
    assert!(view.updater.pulse.borrow().is_none());
    assert_eq!(action.label().as_deref(), Some("Install update…"));
    assert!(!row.has_css_class("error"));
    let toast = view.updater.notice.borrow().as_ref().unwrap().clone();
    assert_eq!(toast.button_label().as_deref(), Some("Copy details"));
    toast.emit_by_name::<()>("button-clicked", &[]);
    let copied = Rc::new(RefCell::new(None));
    action
        .clipboard()
        .read_text_async(None::<&gtk::gio::Cancellable>, {
            let copied = Rc::clone(&copied);
            move |result| {
                copied.replace(Some(result.unwrap().unwrap().to_string()));
            }
        });
    pump_until(|| copied.borrow().is_some());
    let copied = copied.borrow();
    let details = copied.as_deref().unwrap();
    assert!(details.contains("No space left on device"));
    assert!(!details.contains("private-value"));
    assert!(details.contains("partial"));
    assert_eq!(view.updater.row, row);
    assert_eq!(view.updater.action, action);
    assert!(
        intents.borrow().is_empty(),
        "rendering must not submit intents"
    );
    window.set_default_size(360, 700);
    pump_until(|| window.width() <= 360);
    assert!(view.widget().measure(gtk::Orientation::Horizontal, -1).0 <= 360);
    window.close();
}
