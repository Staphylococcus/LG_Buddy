use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use lg_buddy::overview::OverviewIntent;
use lg_buddy::presentation::brightness::{BrightnessStatus, UserFacingError};
use lg_buddy::presentation::overview::{
    AudioStatus, OverviewPresentation, TvConnectionState, TvSummaryStatus,
};

pub(crate) type IntentHandler = Rc<dyn Fn(OverviewIntent)>;

pub(crate) struct OverviewWindow {
    window: adw::ApplicationWindow,
    summary: gtk::Label,
    connection: gtk::Label,
    summary_retry: gtk::Button,
    brightness: SliderRow,
    volume: SliderRow,
    mute: gtk::ToggleButton,
    suppress: Rc<Cell<bool>>,
    initial_brightness_focus: Cell<bool>,
    allow_close: Rc<Cell<bool>>,
    close_requested: Rc<Cell<bool>>,
}

// ponytail: keep the two native rows alive; rendering only changes their values.
struct SliderRow {
    root: gtk::Box,
    scale: gtk::Scale,
    status: gtk::Label,
    retry: gtk::Button,
}

impl SliderRow {
    fn new(
        icon: &impl IsA<gtk::Widget>,
        label: &str,
        retry_intent: OverviewIntent,
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
        let retry = retry_button(&format!("Retry {label}"), retry_intent, on_intent);
        feedback.append(&status);
        feedback.append(&retry);
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

    fn feedback(&self, loading: Option<&str>, error: Option<&UserFacingError>, retry: bool) {
        let message = error.map(error_text).or_else(|| loading.map(str::to_owned));
        self.status.set_text(message.as_deref().unwrap_or(""));
        self.status.set_visible(message.is_some());
        self.status.set_accessible_role(if error.is_some() {
            gtk::AccessibleRole::Alert
        } else {
            gtk::AccessibleRole::Status
        });
        self.retry.set_visible(retry);
    }
}

impl OverviewWindow {
    pub(crate) fn new(application: &adw::Application, on_intent: IntentHandler) -> Self {
        let suppress = Rc::new(Cell::new(false));
        let brightness_icon = gtk::Image::from_icon_name("display-brightness-symbolic");
        brightness_icon.set_pixel_size(20);
        brightness_icon.set_size_request(36, 36);
        brightness_icon.set_tooltip_text(Some("Brightness"));
        let brightness = SliderRow::new(
            &brightness_icon,
            "OLED Pixel Brightness",
            OverviewIntent::RetryBrightness,
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
            OverviewIntent::RetryAudio,
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
        let summary_retry = retry_button(
            "Retry TV configuration",
            OverviewIntent::RetrySummary,
            &on_intent,
        );
        let summary_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        // Match the visible icon edge inside the native controls' hit areas.
        summary_row.set_margin_start(8);
        summary_row.set_margin_end(12);
        summary_row.append(&summary_text);
        summary_row.append(&summary_retry);

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
        let shell = gtk::Box::new(gtk::Orientation::Vertical, 0);
        shell.append(&adw::HeaderBar::new());
        shell.append(&scroller);
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .icon_name(crate::APPLICATION_ID)
            .default_width(420)
            .default_height(240)
            .content(&shell)
            .build();
        let allow_close = Rc::new(Cell::new(false));
        let close_requested = Rc::new(Cell::new(false));
        window.connect_close_request({
            let allow_close = Rc::clone(&allow_close);
            let close_requested = Rc::clone(&close_requested);
            move |_| {
                if allow_close.get() {
                    return gtk::glib::Propagation::Proceed;
                }
                close_requested.set(true);
                on_intent(OverviewIntent::Cancel);
                close_requested.set(false);
                if allow_close.get() {
                    gtk::glib::Propagation::Proceed
                } else {
                    gtk::glib::Propagation::Stop
                }
            }
        });
        Self {
            window,
            summary,
            connection,
            summary_retry,
            brightness,
            volume,
            mute,
            suppress,
            initial_brightness_focus: Cell::new(true),
            allow_close,
            close_requested,
        }
    }

    pub(crate) fn render(&self, presentation: &OverviewPresentation) {
        self.suppress.set(true);
        self.window.set_title(Some(presentation.title()));
        self.summary
            .set_text(&match presentation.summary().status() {
                TvSummaryStatus::Loading { message } | TvSummaryStatus::Ready { message } => {
                    message.clone()
                }
                TvSummaryStatus::Failed(error) => error_text(error),
            });
        self.summary_retry
            .set_visible(presentation.summary().retry_action().is_some());
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
        self.brightness.feedback(loading, error, error.is_some());
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
            audio.retry_action().is_some(),
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
        if self.initial_brightness_focus.get() && brightness.control().is_some() {
            self.initial_brightness_focus.set(false);
            let scale = self.brightness.scale.clone();
            gtk::glib::idle_add_local_once(move || {
                scale.grab_focus();
            });
        }
    }

    pub(crate) fn present(&self) {
        self.window.present();
    }

    pub(crate) fn close(&self) {
        self.allow_close.set(true);
        if !self.close_requested.get() {
            self.window.close();
        }
    }

    #[cfg(test)]
    pub(crate) fn window(&self) -> gtk::Window {
        self.window.clone().upcast()
    }
}

fn retry_button(label: &str, intent: OverviewIntent, on_intent: &IntentHandler) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text(label)
        .build();
    button.add_css_class("flat");
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button.connect_clicked({
        let on_intent = Rc::clone(on_intent);
        move |_| on_intent(intent)
    });
    button
}

fn error_text(error: &UserFacingError) -> String {
    format!("{} {}", error.summary(), error.detail())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lg_buddy::overview::{OverviewApplication, OverviewFrontendUpdate, OverviewOperation};
    use lg_buddy::tv::{AudioStatus, CurrentVolume, OledBrightness, VolumeLevel};
    use std::cell::RefCell;

    fn render(view: &OverviewWindow, transition: lg_buddy::overview::OverviewTransition) {
        let OverviewFrontendUpdate::Present(presentation) = transition.update() else {
            panic!("expected presentation")
        };
        view.render(presentation);
    }

    #[test]
    fn compact_controls_submit_and_keep_focus_without_render_feedback() {
        gtk::init().expect("GTK display required");
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
        let view = OverviewWindow::new(
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
        view.present();
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
        assert!(!view.brightness.retry.is_visible());
        assert!(!view.volume.retry.is_visible());
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
        view.close();
        crate::controller_test_support::run_scenario();
    }
}
