use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use lg_buddy::overview::OverviewIntent;
use lg_buddy::presentation::brightness::{BrightnessStatus, UserFacingError};
use lg_buddy::presentation::overview::{
    AudioStatus, OverviewAction, OverviewPresentation, TvConnectionState, TvSummaryStatus,
};

pub(crate) type IntentHandler = Rc<dyn Fn(OverviewIntent)>;

pub(crate) struct OverviewView {
    window: adw::ApplicationWindow,
    root: gtk::ScrolledWindow,
    body: gtk::Box,
    summary: gtk::Label,
    connection: gtk::Label,
    summary_retry: RetryButton,
    brightness: SliderRow,
    volume: SliderRow,
    mute: gtk::ToggleButton,
    suppress: Rc<Cell<bool>>,
    initial_brightness_focus: Rc<Cell<bool>>,
}

// ponytail: keep the two native rows alive; rendering only changes their values.
struct SliderRow {
    root: gtk::Box,
    scale: gtk::Scale,
    status: gtk::Label,
    retry: RetryButton,
}

impl SliderRow {
    fn new(
        icon: &impl IsA<gtk::Widget>,
        label: &str,
        changed: fn(u8) -> OverviewIntent,
        suppress: &Rc<Cell<bool>>,
        on_intent: &IntentHandler,
    ) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        icon.set_valign(gtk::Align::Start);
        root.append(icon);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.set_hexpand(true);
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        scale.set_draw_value(false);
        scale.set_height_request(36);
        scale.set_hexpand(true);
        scale.update_property(&[gtk::accessible::Property::Label(label)]);
        scale.connect_value_changed({
            let suppress = Rc::clone(suppress);
            let on_intent = Rc::clone(on_intent);
            move |scale| {
                if !suppress.get() {
                    on_intent(changed(scale.value().round() as u8));
                }
            }
        });
        content.append(&scale);
        let feedback = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let status = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .build();
        let retry = RetryButton::new(on_intent);
        feedback.append(&status);
        feedback.append(&retry.button);
        content.append(&feedback);
        root.append(&content);
        Self {
            root,
            scale,
            status,
            retry,
        }
    }

    fn value(&self, value: Option<(u8, u8, u8, u8, bool)>, description: &str) {
        self.scale.set_visible(value.is_some());
        if let Some((value, minimum, maximum, step, enabled)) = value {
            self.scale.set_range(minimum.into(), maximum.into());
            self.scale.set_increments(step.into(), step.into());
            self.scale.set_value(value.into());
            self.scale.set_sensitive(enabled);
        }
        self.scale.set_tooltip_text(Some(description));
        self.scale
            .update_property(&[gtk::accessible::Property::Description(description)]);
    }

    fn feedback(
        &self,
        loading: Option<&str>,
        error: Option<&UserFacingError>,
        retry: Option<&OverviewAction>,
    ) {
        let message = error.map(error_text).or_else(|| loading.map(str::to_owned));
        self.status.set_text(message.as_deref().unwrap_or(""));
        self.status.set_visible(message.is_some());
        self.status.set_accessible_role(if error.is_some() {
            gtk::AccessibleRole::Alert
        } else {
            gtk::AccessibleRole::Status
        });
        self.retry.render(retry);
    }
}

impl OverviewView {
    pub(crate) fn new(window: &adw::ApplicationWindow, on_intent: IntentHandler) -> Self {
        let suppress = Rc::new(Cell::new(false));
        let brightness_icon = gtk::Image::from_icon_name("display-brightness-symbolic");
        brightness_icon.set_pixel_size(20);
        brightness_icon.set_size_request(36, 36);
        brightness_icon.set_tooltip_text(Some("Brightness"));
        let brightness = SliderRow::new(
            &brightness_icon,
            "OLED Pixel Brightness",
            OverviewIntent::SetBrightness,
            &suppress,
            &on_intent,
        );
        let mute = gtk::ToggleButton::builder()
            .icon_name("audio-volume-high-symbolic")
            .width_request(36)
            .height_request(36)
            .build();
        mute.add_css_class("flat");
        mute.connect_toggled({
            let suppress = Rc::clone(&suppress);
            let on_intent = Rc::clone(&on_intent);
            move |button| {
                if !suppress.get() {
                    on_intent(OverviewIntent::SetMuted(button.is_active()));
                }
            }
        });
        let volume = SliderRow::new(
            &mute,
            "TV Volume",
            OverviewIntent::SetVolume,
            &suppress,
            &on_intent,
        );
        let summary = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .build();
        summary.add_css_class("dim-label");
        let connection = gtk::Label::builder()
            .xalign(0.0)
            .accessible_role(gtk::AccessibleRole::Status)
            .build();
        let summary_text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        summary_text.append(&summary);
        summary_text.append(&connection);
        let summary_retry = RetryButton::new(&on_intent);
        let summary_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        // Match the visible icon edge inside the native controls' hit areas.
        summary_row.set_margin_start(8);
        summary_row.set_margin_end(12);
        summary_row.append(&summary_text);
        summary_row.append(&summary_retry.button);

        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(16)
            .margin_bottom(20)
            .build();
        body.append(&summary_row);
        body.append(&brightness.root);
        body.append(&volume.root);
        let clamp = adw::Clamp::builder()
            .maximum_size(600)
            .tightening_threshold(400)
            .margin_start(20)
            .margin_end(20)
            .child(&body)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&clamp)
            .build();
        Self {
            window: window.clone(),
            root: scroller,
            body,
            summary,
            connection,
            summary_retry,
            brightness,
            volume,
            mute,
            suppress,
            initial_brightness_focus: Rc::new(Cell::new(true)),
        }
    }

    pub(crate) fn render(&self, presentation: &OverviewPresentation) {
        self.suppress.set(true);
        self.summary
            .set_text(&match presentation.summary().status() {
                TvSummaryStatus::Loading { message } | TvSummaryStatus::Ready { message } => {
                    message.clone()
                }
                TvSummaryStatus::Failed(error) => error_text(error),
            });
        self.summary_retry
            .render(presentation.summary().retry_action());
        let connection = presentation.summary().connection_state();
        self.connection
            .set_text(&format!("● {}", connection.label()));
        self.connection
            .update_property(&[gtk::accessible::Property::Label(connection.label())]);
        for class in ["success", "warning", "error"] {
            self.connection.remove_css_class(class);
        }
        self.connection.add_css_class(match connection {
            TvConnectionState::Connected => "success",
            TvConnectionState::Connecting => "warning",
            TvConnectionState::Disconnected => "error",
        });
        let brightness = presentation.brightness();
        let (loading, error, description) = match brightness.status() {
            BrightnessStatus::Loading { message } => {
                (Some(message.as_str()), None, message.clone())
            }
            BrightnessStatus::Ready { message } | BrightnessStatus::Applying { message } => {
                (None, None, message.clone())
            }
            BrightnessStatus::Failed(error) => (None, Some(error), error_text(error)),
        };
        self.brightness.value(
            brightness.control().map(|c| {
                (
                    c.proposed().as_percent(),
                    c.minimum(),
                    c.maximum(),
                    c.step(),
                    c.enabled(),
                )
            }),
            &description,
        );
        self.brightness.feedback(
            loading,
            error,
            presentation.brightness_retry_action().as_ref(),
        );
        let audio = presentation.audio();
        let (loading, error, description) = match audio.status() {
            AudioStatus::Loading { message } => (Some(message.as_str()), None, message.clone()),
            AudioStatus::Ready { message } | AudioStatus::Applying { message } => {
                (None, None, message.clone())
            }
            AudioStatus::Failed(error) => (None, Some(error), error_text(error)),
        };
        self.volume.value(
            audio.volume().map(|c| {
                (
                    c.proposed().as_percent(),
                    c.minimum(),
                    c.maximum(),
                    c.step(),
                    c.enabled(),
                )
            }),
            &description,
        );
        self.volume.feedback(
            loading.or_else(|| {
                (audio.volume().is_none() && error.is_none()).then_some(description.as_str())
            }),
            error,
            audio.retry_action(),
        );
        let muted = audio.mute().is_some_and(|mute| mute.proposed());
        self.mute.set_active(muted);
        self.mute
            .set_sensitive(audio.mute().is_some_and(|mute| mute.enabled()));
        self.mute.set_icon_name(if muted {
            "audio-volume-muted-symbolic"
        } else {
            "audio-volume-high-symbolic"
        });
        let action = if muted { "Unmute TV" } else { "Mute TV" };
        self.mute.set_tooltip_text(Some(action));
        self.mute
            .update_property(&[gtk::accessible::Property::Label(action)]);
        self.suppress.set(false);
        if matches!(brightness.status(), BrightnessStatus::Failed(_)) {
            self.initial_brightness_focus.set(false);
        } else if self.initial_brightness_focus.get() && brightness.control().is_some() {
            let requested = Rc::clone(&self.initial_brightness_focus);
            let scale = self.brightness.scale.clone();
            let body = self.body.clone();
            let window = self.window.clone();
            gtk::glib::idle_add_local_once(move || {
                // Keep native control focus, including changes since this was queued.
                if requested.replace(false)
                    && window.is_visible()
                    && gtk::prelude::GtkWindowExt::focus(&window)
                        .is_none_or(|focus| !focus.is_ancestor(&body))
                {
                    scale.grab_focus();
                }
            });
        }
    }

    pub(crate) fn widget(&self) -> &gtk::ScrolledWindow {
        &self.root
    }

    pub(crate) fn leave(&self) {
        self.initial_brightness_focus.set(false);
    }

    pub(crate) fn focus_brightness(&self) {
        self.initial_brightness_focus.set(true);
        if self.brightness.scale.is_visible()
            && self.brightness.scale.is_sensitive()
            && self.brightness.scale.grab_focus()
        {
            self.initial_brightness_focus.set(false);
        } else {
            // Let the deferred request yield only to focus chosen after activation.
            gtk::prelude::GtkWindowExt::set_focus(&self.window, None::<&gtk::Widget>);
        }
    }
}

struct RetryButton {
    button: gtk::Button,
    intent: Rc<Cell<Option<OverviewIntent>>>,
}

impl RetryButton {
    fn new(on_intent: &IntentHandler) -> Self {
        let button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .visible(false)
            .sensitive(false)
            .build();
        button.add_css_class("flat");
        let intent = Rc::new(Cell::new(None));
        button.connect_clicked({
            let on_intent = Rc::clone(on_intent);
            let intent = Rc::clone(&intent);
            move |_| {
                if let Some(intent) = intent.get() {
                    on_intent(intent);
                }
            }
        });
        Self { button, intent }
    }

    fn render(&self, action: Option<&OverviewAction>) {
        self.intent.set(
            action
                .filter(|action| action.enabled())
                .map(OverviewAction::intent),
        );
        self.button.set_visible(action.is_some());
        self.button
            .set_sensitive(action.is_some_and(OverviewAction::enabled));
        self.button
            .set_tooltip_text(action.map(OverviewAction::label));
        self.button
            .update_property(&[gtk::accessible::Property::Label(
                action.map_or("", OverviewAction::label),
            )]);
    }
}

fn error_text(error: &UserFacingError) -> String {
    format!("{} {}", error.summary(), error.detail())
}

#[cfg(test)]
mod tests {
    use super::*;
    use adw::prelude::AdwApplicationWindowExt;
    use lg_buddy::overview::{OverviewApplication, OverviewFrontendUpdate, OverviewOperation};
    use lg_buddy::tv::{AudioStatus, CurrentVolume, OledBrightness, VolumeLevel};
    use std::cell::RefCell;

    fn test_view(application: &adw::Application, handler: IntentHandler) -> OverviewView {
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("LG Buddy")
            .default_width(420)
            .default_height(240)
            .build();
        let view = OverviewView::new(&window, handler);
        window.set_content(Some(view.widget()));
        view
    }

    fn render(view: &OverviewView, transition: lg_buddy::overview::OverviewTransition) {
        let OverviewFrontendUpdate::Present(presentation) = transition.update() else {
            panic!("expected presentation")
        };
        view.render(presentation);
    }

    fn retry_button_follows_the_latest_declaration() {
        let intents = Rc::new(RefCell::new(Vec::new()));
        let on_intent: IntentHandler = Rc::new({
            let intents = Rc::clone(&intents);
            move |intent| intents.borrow_mut().push(intent)
        });
        let retry = RetryButton::new(&on_intent);
        for (label, intent) in [
            ("Retry brightness read", OverviewIntent::RetryBrightness),
            ("Retry unmuting", OverviewIntent::RetryAudio),
        ] {
            retry.render(Some(&OverviewAction::new(label, true, intent)));
            assert!(retry.button.is_visible());
            assert!(retry.button.is_sensitive());
            assert_eq!(retry.button.tooltip_text().as_deref(), Some(label));
            retry.button.emit_clicked();
        }
        retry.render(Some(&OverviewAction::new(
            "Retry unavailable",
            false,
            OverviewIntent::RetrySummary,
        )));
        assert!(retry.button.is_visible());
        assert!(!retry.button.is_sensitive());
        retry.button.emit_clicked();
        retry.render(Some(&OverviewAction::new(
            "Retry configuration",
            true,
            OverviewIntent::RetrySummary,
        )));
        retry.render(None);
        assert!(!retry.button.is_visible());
        assert!(retry.button.tooltip_text().is_none());
        retry.button.emit_clicked();
        assert_eq!(
            *intents.borrow(),
            [OverviewIntent::RetryBrightness, OverviewIntent::RetryAudio],
            "disabled and absent actions must not retain a callback"
        );
    }

    fn late_brightness_respects_focus(application: &adw::Application) {
        // An explicit activation replaces earlier focus, but later user choices
        // win, including during a retry or after the idle callback was queued.
        for (retry_read, explicit_request, focus_after_render) in [
            (false, false, None),
            (false, false, Some(false)),
            (true, false, Some(false)),
            (false, false, Some(true)),
            (false, true, None),
            (true, true, None),
            (false, true, Some(false)),
            (false, true, Some(true)),
        ] {
            let view = test_view(application, Rc::new(|_| {}));
            let (mut app, opening) = OverviewApplication::open();
            render(&view, opening.clone());
            view.window.present();
            let OverviewOperation::ReadAudio(audio_op) = opening.operations()[2] else {
                unreachable!()
            };
            render(
                &view,
                app.complete_audio_read(
                    audio_op,
                    Ok(AudioStatus::new(
                        CurrentVolume::Level(VolumeLevel::new(20).unwrap()),
                        false,
                    )),
                )
                .unwrap(),
            );
            let OverviewOperation::ReadBrightness(mut brightness_op) = opening.operations()[1]
            else {
                unreachable!()
            };
            if retry_read {
                render(
                    &view,
                    app.complete_brightness_read(
                        brightness_op,
                        Err(lg_buddy::brightness::BrightnessReadError::new(
                            lg_buddy::brightness::BrightnessReadFailure::Unreachable,
                            "planned read failure",
                        )),
                    )
                    .unwrap(),
                );
                let retry = app.handle_intent(OverviewIntent::RetryBrightness).unwrap();
                let OverviewOperation::ReadBrightness(op) = retry.operations()[0] else {
                    unreachable!()
                };
                brightness_op = op;
                render(&view, retry);
            }
            while gtk::glib::MainContext::default().pending() {
                gtk::glib::MainContext::default().iteration(false);
            }
            if explicit_request {
                assert!(view.volume.scale.grab_focus());
                view.focus_brightness();
            }
            if focus_after_render == Some(false) {
                assert!(view.volume.scale.grab_focus());
            }
            render(
                &view,
                app.complete_brightness_read(brightness_op, Ok(OledBrightness::new(50).unwrap()))
                    .unwrap(),
            );
            if focus_after_render == Some(true) {
                assert!(view.volume.scale.grab_focus());
            }
            while gtk::glib::MainContext::default().pending() {
                gtk::glib::MainContext::default().iteration(false);
            }
            if focus_after_render.is_some() {
                assert!(view.volume.scale.has_focus(), "brightness stole volume focus: retry={retry_read}, explicit_request={explicit_request}, focus_after_render={focus_after_render:?}");
            } else {
                assert!(
                    view.brightness.scale.has_focus(),
                    "brightness focus was lost: retry={retry_read}, explicit_request={explicit_request}"
                );
            }
            view.window.close();
        }
    }

    #[test]
    fn compact_controls_submit_and_keep_focus_without_render_feedback() {
        gtk::init().expect("GTK display required");
        retry_button_follows_the_latest_declaration();
        let application = adw::Application::builder()
            .application_id(format!(
                "{}.RendererTest{}",
                crate::APPLICATION_ID,
                std::process::id()
            ))
            .build();
        application
            .register(None::<&gtk::gio::Cancellable>)
            .unwrap();
        let intents = Rc::new(RefCell::new(Vec::new()));
        let view = test_view(
            &application,
            Rc::new({
                let intents = Rc::clone(&intents);
                move |intent| intents.borrow_mut().push(intent)
            }),
        );
        let (mut app, opening) = OverviewApplication::open();
        render(&view, opening.clone());
        assert_eq!(view.connection.text(), "● Connecting");
        assert!(view.connection.has_css_class("warning"));
        view.window.present();
        for operation in opening.operations() {
            match *operation {
                OverviewOperation::ReadSummary(op) => render(
                    &view,
                    app.complete_summary(
                        op,
                        Ok(lg_buddy::overview::OverviewTvIdentity::new(
                            std::net::Ipv4Addr::LOCALHOST,
                            lg_buddy::config::HdmiInput::Hdmi1,
                            lg_buddy::config::TvPlatform::Bscpylgtv,
                        )),
                    )
                    .unwrap(),
                ),
                OverviewOperation::ReadBrightness(op) => render(
                    &view,
                    app.complete_brightness_read(op, Ok(OledBrightness::new(50).unwrap()))
                        .unwrap(),
                ),
                OverviewOperation::ReadAudio(op) => render(
                    &view,
                    app.complete_audio_read(
                        op,
                        Ok(AudioStatus::new(
                            CurrentVolume::Level(VolumeLevel::new(20).unwrap()),
                            true,
                        )),
                    )
                    .unwrap(),
                ),
                _ => {}
            }
        }
        while gtk::glib::MainContext::default().pending() {
            gtk::glib::MainContext::default().iteration(false);
        }
        assert_eq!(view.window.title().as_deref(), Some("LG Buddy"));
        assert_eq!(view.connection.text(), "● Connected");
        assert!(view.connection.has_css_class("success"));
        assert!(view.brightness.scale.has_focus());
        assert!(
            intents.borrow().is_empty(),
            "rendering must never submit a control"
        );
        assert!(!view.brightness.scale.draws_value());
        assert!(!view.volume.scale.draws_value());
        assert!(!view.brightness.status.is_visible());
        assert!(!view.volume.status.is_visible());
        assert!(!view.brightness.retry.button.is_visible());
        assert!(!view.volume.retry.button.is_visible());
        assert_eq!(
            view.mute.icon_name().as_deref(),
            Some("audio-volume-muted-symbolic")
        );
        assert_eq!(
            view.brightness.scale.accessible_role(),
            gtk::AccessibleRole::Slider
        );
        let theme = gtk::IconTheme::for_display(&gtk::prelude::WidgetExt::display(&view.window));
        for icon in [
            "display-brightness-symbolic",
            "audio-volume-muted-symbolic",
            "audio-volume-high-symbolic",
        ] {
            assert!(theme.has_icon(icon), "missing native icon: {icon}");
        }
        view.brightness.scale.set_value(55.0);
        view.volume.scale.set_value(21.0);
        view.mute.set_active(false);
        assert_eq!(
            *intents.borrow(),
            [
                OverviewIntent::SetBrightness(55),
                OverviewIntent::SetVolume(21),
                OverviewIntent::SetMuted(false)
            ]
        );
        let transition = app.handle_intent(OverviewIntent::SetMuted(false)).unwrap();
        render(&view, transition.clone());
        assert_eq!(
            view.mute.icon_name().as_deref(),
            Some("audio-volume-high-symbolic")
        );
        assert!(view.brightness.scale.has_focus());
        assert_eq!(
            intents.borrow().len(),
            3,
            "updating the mute icon must not submit again"
        );
        let style = adw::StyleManager::default();
        let original = style.color_scheme();
        style.set_color_scheme(adw::ColorScheme::ForceDark);
        render(&view, transition.clone());
        assert!(style.is_dark());
        style.set_color_scheme(adw::ColorScheme::ForceLight);
        render(&view, transition);
        assert!(!style.is_dark());
        style.set_color_scheme(original);
        let (mut disconnected, opening) = OverviewApplication::open();
        let OverviewOperation::ReadSummary(op) = opening.operations()[0] else {
            unreachable!()
        };
        render(
            &view,
            disconnected
                .complete_summary(
                    op,
                    Err(lg_buddy::overview::OverviewSummaryError::new(
                        lg_buddy::overview::OverviewSummaryFailure::NotConfigured,
                        "not configured",
                    )),
                )
                .unwrap(),
        );
        assert_eq!(view.connection.text(), "● Disconnected");
        assert!(view.connection.has_css_class("error"));
        let OverviewOperation::ReadBrightness(op) = opening.operations()[1] else {
            unreachable!()
        };
        render(
            &view,
            disconnected
                .complete_brightness_read(
                    op,
                    Err(lg_buddy::brightness::BrightnessReadError::new(
                        lg_buddy::brightness::BrightnessReadFailure::Internal,
                        "planned read failure",
                    )),
                )
                .unwrap(),
        );
        let OverviewOperation::ReadAudio(op) = opening.operations()[2] else {
            unreachable!()
        };
        let failed = disconnected
            .complete_audio_read(
                op,
                Err(lg_buddy::overview::AudioReadError::new(
                    lg_buddy::overview::AudioReadFailure::Internal,
                    "planned read failure",
                )),
            )
            .unwrap();
        let OverviewFrontendUpdate::Present(presentation) = failed.update() else {
            unreachable!()
        };
        view.render(presentation);
        intents.borrow_mut().clear();
        for (retry, action) in [
            (
                &view.summary_retry,
                presentation.summary().retry_action().unwrap().clone(),
            ),
            (
                &view.brightness.retry,
                presentation.brightness_retry_action().unwrap(),
            ),
            (
                &view.volume.retry,
                presentation.audio().retry_action().unwrap().clone(),
            ),
        ] {
            assert!(retry.button.is_visible());
            assert_eq!(retry.button.is_sensitive(), action.enabled());
            assert_eq!(retry.button.tooltip_text().as_deref(), Some(action.label()));
            retry.button.emit_clicked();
            assert_eq!(intents.borrow().last(), Some(&action.intent()));
        }
        let retry = presentation.brightness_retry_action().unwrap().intent();
        render(&view, disconnected.handle_intent(retry).unwrap());
        assert!(!view.brightness.retry.button.is_visible());
        assert!(view.volume.retry.button.is_visible());
        view.window.close();
        late_brightness_respects_focus(&application);
        crate::tvs::run_renderer_scenarios(&application);
        crate::settings::run_renderer_scenarios(&application);
        crate::diagnostics::run_renderer_scenarios(&application);
        crate::controller_test_support::run_scenario();
    }
}
