use adw::prelude::*;
use lg_buddy::setup::gui::{OnboardingIntent, OnboardingPresentation};
use std::{cell::Cell, rc::Rc};

/// The window's central setup gate renders the shared repair flow.
pub(crate) struct OnboardingView {
    pub(crate) root: adw::ToolbarView,
    title: gtk::Label,
    description: gtk::Label,
    form: crate::pairing::PairingForm,
    status: gtk::Label,
    primary: gtk::Button,
    cancel: gtk::Button,
    progress: gtk::Spinner,
    presented: Cell<bool>,
}
impl OnboardingView {
    pub fn new(on_intent: Rc<dyn Fn(OnboardingIntent)>) -> Self {
        let description = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .build();
        description.set_accessible_role(gtk::AccessibleRole::Status);
        let status = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .build();
        status.set_accessible_role(gtk::AccessibleRole::Alert);
        status.add_css_class("error");
        let form = crate::pairing::PairingForm::new(on_intent.clone());
        let primary = gtk::Button::new();
        primary.add_css_class("suggested-action");
        primary.connect_clicked({
            let on_intent = on_intent.clone();
            move |_| on_intent(OnboardingIntent::Submit)
        });
        let cancel = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Close")
            .build();
        cancel.update_property(&[gtk::accessible::Property::Label("Close")]);
        cancel.connect_clicked({
            let on_intent = on_intent.clone();
            move |_| on_intent(OnboardingIntent::Cancel)
        });
        let progress = gtk::Spinner::builder()
            .halign(gtk::Align::Center)
            .visible(false)
            .build();
        progress.update_property(&[gtk::accessible::Property::Label("Setup in progress")]);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(20)
            .margin_bottom(20)
            .build();
        let title = gtk::Label::builder().xalign(0.0).wrap(true).build();
        title.add_css_class("title-2");
        content.append(&title);
        content.append(&description);
        content.append(&status);
        content.append(&form.root);
        content.append(&progress);
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
            .title_widget(&adw::WindowTitle::new(
                "Complete setup",
                crate::APPLICATION_NAME,
            ))
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        header.pack_start(&cancel);
        header.pack_end(&primary);
        let toolbar = adw::ToolbarView::builder().content(&scroller).build();
        toolbar.add_top_bar(&header);
        let diagnostics = gtk::Button::builder()
            .icon_name("dialog-information-symbolic")
            .tooltip_text("Diagnostics")
            .action_name("win.diagnostics")
            .build();
        diagnostics.update_property(&[gtk::accessible::Property::Label("Diagnostics")]);
        header.pack_end(&diagnostics);
        Self {
            root: toolbar,
            title,
            description,
            form,
            status,
            primary,
            cancel,
            progress,
            presented: Cell::new(false),
        }
    }
    pub fn render(
        &self,
        _parent: &adw::ApplicationWindow,
        presentation: Option<&OnboardingPresentation>,
    ) {
        let Some(view) = presentation else {
            self.progress.stop();
            self.presented.set(false);
            return;
        };
        let had_form = self.form.root.is_visible();
        self.title.set_text(&view.title);
        self.description.set_text(&view.description);
        self.form.root.set_visible(view.pairing.is_some());
        if let Some(pairing) = &view.pairing {
            self.form.render(pairing, !view.busy);
        }
        self.primary.set_label(view.action.unwrap_or("Continue"));
        self.primary.set_visible(view.action.is_some());
        self.primary
            .set_sensitive(!view.busy && view.action.is_some());
        self.cancel.set_sensitive(view.can_cancel);
        self.status.set_visible(view.error.is_some());
        self.status.set_text(
            &view
                .error
                .as_ref()
                .map(|e| format!("{}: {}", e.summary(), e.detail()))
                .unwrap_or_default(),
        );
        self.progress.set_visible(view.busy);
        self.progress.set_spinning(view.busy);
        let newly_presented = !self.presented.replace(true);
        if view.pairing.is_some() && (newly_presented || !had_form) {
            self.form.focus();
        }
    }
}

#[cfg(test)]
pub(crate) fn run_renderer_scenarios(application: &adw::Application) {
    use lg_buddy::setup::{flow::SetupStep, gui::OnboardingPresentation, StepInput, StepResponse};
    use std::cell::RefCell;
    let intents = Rc::new(RefCell::new(Vec::new()));
    let view = OnboardingView::new(Rc::new({
        let intents = intents.clone();
        move |intent| intents.borrow_mut().push(intent)
    }));
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .build();
    window.present();
    let pairing = OnboardingPresentation::for_step(
        SetupStep::Pairing,
        &StepResponse::InputRequired(StepInput::Pairing { saved: None }),
    );
    view.render(&window, Some(&pairing));
    assert_eq!(view.title.text(), "Pair a TV");
    assert!(view.form.root.is_visible());
    view.primary.emit_clicked();
    assert_eq!(intents.borrow_mut().pop(), Some(OnboardingIntent::Submit));
    let services = OnboardingPresentation::for_step(
        SetupStep::Services,
        &StepResponse::ActionRequired {
            explanation: "Install background services.",
            requires_authorization: true,
        },
    );
    view.render(&window, Some(&services));
    assert!(!view.form.root.is_visible());
    assert!(view.description.text().contains("password"));
    let running = OnboardingPresentation::for_step(
        SetupStep::Services,
        &StepResponse::Running {
            message: "Installing services…",
            cancelable: false,
        },
    );
    view.render(&window, Some(&running));
    assert!(view.progress.is_spinning());
    assert!(!view.cancel.is_sensitive());
    if view.cancel.is_sensitive() {
        view.cancel.emit_clicked();
    }
    assert!(intents.borrow().is_empty());
    let deps = OnboardingPresentation::for_step(
        SetupStep::Plasma,
        &StepResponse::InputRequired(StepInput::BuildDependencies {
            explanation: "Install compiler packages?",
        }),
    );
    view.render(&window, Some(&deps));
    assert_eq!(view.primary.label().as_deref(), Some("Install build tools"));
    view.cancel.emit_clicked();
    assert_eq!(intents.borrow_mut().pop(), Some(OnboardingIntent::Cancel));
    view.render(&window, None);
    assert!(!view.presented.get());
    window.close();
}
