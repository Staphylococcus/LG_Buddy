mod overview;
mod pairing;
mod tvs;
mod window;

use std::cell::{Cell, RefCell};
use std::fmt;
use std::rc::Rc;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use lg_buddy::application::{Application, ApplicationTransition, OverviewCompletion};
use lg_buddy::audio::{AudioWriteError, AudioWriteFailure};
use lg_buddy::brightness::{
    BrightnessReadError, BrightnessReadFailure, BrightnessWriteError, BrightnessWriteFailure,
};
use lg_buddy::navigation::{ApplicationPage, Navigation};
use lg_buddy::overview::{
    EnvironmentOverviewBackend, OverviewBackend, OverviewFrontendUpdate, OverviewIntent,
    OverviewOperation, OverviewSummaryError, OverviewTransition,
};
use lg_buddy::pairing::{
    EnvironmentPairingBackend, PairingBackend, PairingError, PairingOperation, PairingStage,
};
use lg_buddy::tvs::{
    EnvironmentTvsBackend, TvsBackend, TvsIntent, TvsModelReadOperation, TvsReadError,
    TvsReadOperation, TvsTransition,
};

pub const APPLICATION_ID: &str = "io.github.staphylococcus.LGBuddy";
pub const APPLICATION_NAME: &str = "LG Buddy";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiCommand {
    Brightness,
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuiParseError {
    MissingCommand,
    UnknownCommand(String),
    UnexpectedArguments(Vec<String>),
}

impl fmt::Display for GuiParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCommand => write!(f, "missing command; expected `brightness`"),
            Self::UnknownCommand(command) => write!(f, "unknown command `{command}`"),
            Self::UnexpectedArguments(arguments) => {
                write!(f, "unexpected arguments: {}", arguments.join(" "))
            }
        }
    }
}

pub fn parse_args<I, S>(args: I) -> Result<GuiCommand, GuiParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let command = match args.next() {
        Some(command) if command.as_ref() == "brightness" => GuiCommand::Brightness,
        Some(command) if matches!(command.as_ref(), "--version" | "-V") => GuiCommand::Version,
        Some(command) => return Err(GuiParseError::UnknownCommand(command.as_ref().to_string())),
        None => return Err(GuiParseError::MissingCommand),
    };
    let unexpected: Vec<String> = args.map(|argument| argument.as_ref().to_string()).collect();
    if !unexpected.is_empty() {
        return Err(GuiParseError::UnexpectedArguments(unexpected));
    }
    Ok(command)
}

pub fn help(program: &str) -> String {
    format!("Usage: {program} brightness\n       {program} --version\n")
}

pub fn run(command: GuiCommand) -> glib::ExitCode {
    match command {
        GuiCommand::Brightness => run_application(),
        GuiCommand::Version => {
            print!("{}", lg_buddy::version::version_text());
            glib::ExitCode::SUCCESS
        }
    }
}

fn run_application() -> glib::ExitCode {
    glib::set_application_name(APPLICATION_NAME);
    let application = adw::Application::builder()
        .application_id(APPLICATION_ID)
        .build();
    let controller = Rc::new(RefCell::new(None::<Rc<ApplicationController>>));
    install_application_actions(&application, Rc::clone(&controller));
    connect_application(
        &application,
        Rc::clone(&controller),
        Arc::new(EnvironmentOverviewBackend),
        Arc::new(EnvironmentTvsBackend),
    );
    application.run_with_args(&["lg-buddy-gui"])
}

fn install_application_actions(
    application: &adw::Application,
    controller: Rc<RefCell<Option<Rc<ApplicationController>>>>,
) {
    let quit = gtk::gio::SimpleAction::new("quit", None);
    let application_for_quit = application.clone();
    quit.connect_activate({
        let controller = Rc::clone(&controller);
        move |_, _| {
            if let Some(controller) = controller.borrow().as_ref() {
                ApplicationController::handle_intent(controller, OverviewIntent::Cancel);
            } else {
                application_for_quit.quit();
            }
        }
    });
    application.add_action(&quit);
    application.set_accels_for_action("app.quit", &["<Primary>q"]);
    let escape = gtk::gio::SimpleAction::new("escape", None);
    let application_for_escape = application.clone();
    escape.connect_activate(move |_, _| {
        if let Some(controller) = controller.borrow().as_ref() {
            if !controller.window.dismiss_dialog() {
                ApplicationController::handle_intent(controller, OverviewIntent::Cancel);
            }
        } else {
            application_for_escape.quit();
        }
    });
    application.add_action(&escape);
    application.set_accels_for_action("app.escape", &["Escape"]);
}

struct ApplicationController {
    application: RefCell<Application>,
    gtk_application: adw::Application,
    window: window::ApplicationWindow,
    tvs_backend: Arc<dyn TvsBackend>,
    pairing_backend: Arc<dyn PairingBackend>,
    navigation: RefCell<Navigation>,
    backend: Arc<dyn OverviewBackend>,
    closed: Cell<bool>,
}

impl ApplicationController {
    fn new(
        gtk_application: &adw::Application,
        backend: Arc<dyn OverviewBackend>,
        tvs_backend: Arc<dyn TvsBackend>,
    ) -> (Rc<Self>, ApplicationTransition) {
        Self::with_pairing_backend(
            gtk_application,
            backend,
            tvs_backend,
            Arc::new(EnvironmentPairingBackend),
        )
    }

    fn with_pairing_backend(
        gtk_application: &adw::Application,
        backend: Arc<dyn OverviewBackend>,
        tvs_backend: Arc<dyn TvsBackend>,
        pairing_backend: Arc<dyn PairingBackend>,
    ) -> (Rc<Self>, ApplicationTransition) {
        let (application, opening) = Application::open();
        let controller = Rc::new_cyclic(|controller| {
            let on_intent: overview::IntentHandler = Rc::new({
                let controller = controller.clone();
                move |intent| {
                    if let Some(controller) = controller.upgrade() {
                        Self::handle_intent(&controller, intent);
                    }
                }
            });
            let on_tvs = Rc::new({
                let controller = controller.clone();
                move |intent| {
                    if let Some(controller) = controller.upgrade() {
                        Self::handle_tvs_intent(&controller, intent);
                    }
                }
            });
            let on_navigation = Rc::new({
                let controller = controller.clone();
                move |page| {
                    if let Some(controller) = controller.upgrade() {
                        controller.navigate(page);
                    }
                }
            });
            Self {
                tvs_backend,
                pairing_backend,
                navigation: RefCell::new(Navigation::default()),
                application: RefCell::new(application),
                gtk_application: gtk_application.clone(),
                window: window::ApplicationWindow::new(
                    gtk_application,
                    on_intent,
                    on_tvs,
                    on_navigation,
                ),
                backend,
                closed: Cell::new(false),
            }
        });
        (controller, opening)
    }

    fn present(&self) {
        if !self.closed.get() {
            self.window.present();
        }
    }

    fn handle_intent(controller: &Rc<Self>, intent: OverviewIntent) {
        let transition = controller
            .application
            .borrow_mut()
            .handle_overview_intent(intent);
        if let Some(transition) = transition {
            Self::apply_transition(controller, transition);
        }
    }

    fn apply_transition(controller: &Rc<Self>, transition: ApplicationTransition) {
        if let Some(tvs) = transition.tvs() {
            Self::render_tvs_transition(controller, tvs);
        }
        if let Some(overview) = transition.overview() {
            Self::render_overview_transition(controller, overview);
        }
    }

    fn render_overview_transition(controller: &Rc<Self>, transition: &OverviewTransition) {
        if let Some(diagnostic) = transition.diagnostic() {
            eprintln!("LG Buddy GUI: {diagnostic}");
        }
        match transition.update() {
            OverviewFrontendUpdate::Present(presentation) => {
                controller.window.render(presentation);
            }
            OverviewFrontendUpdate::Close => {
                controller.closed.set(true);
                controller.window.close();
            }
        }
        for operation in transition.operations() {
            Self::start_operation(controller, *operation);
        }
    }

    fn navigate(&self, page: ApplicationPage) {
        if !self.closed.get() {
            self.navigation.borrow_mut().select(page);
            self.window.navigate(self.navigation.borrow().selected());
        }
    }

    fn handle_tvs_intent(controller: &Rc<Self>, intent: TvsIntent) {
        let transition = controller
            .application
            .borrow_mut()
            .handle_tvs_intent(intent);
        if let Some(transition) = transition {
            Self::apply_transition(controller, transition);
        }
    }

    fn render_tvs_transition(controller: &Rc<Self>, transition: &TvsTransition) {
        if let Some(diagnostic) = transition.diagnostic() {
            eprintln!("LG Buddy GUI: {diagnostic}");
        }
        controller.window.render_tvs(transition.presentation());
        if let Some(message) = transition.toast_message() {
            controller.window.show_toast(message);
        }
        if let Some(operation) = transition.read_operation() {
            Self::start_tvs_read(controller, operation);
        }
        if let Some(operation) = transition.model_read_operation() {
            Self::start_tvs_model_read(controller, operation.clone());
        }
        if let Some(operation) = transition.pairing_operation() {
            Self::start_pairing(controller, operation.clone());
        }
        if let Some(operation) = transition.management_operation() {
            Self::start_tvs_management(controller, operation.clone());
        }
    }

    fn start_tvs_management(
        controller: &Rc<Self>,
        operation: lg_buddy::tvs::TvsManagementOperation,
    ) {
        let backend = Arc::clone(&controller.tvs_backend);
        let worker_operation = operation.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut application_hold = Some(controller.gtk_application.hold());
        thread::spawn(move || {
            let _ = sender.send(backend.manage(&worker_operation));
        });
        let controller = Rc::downgrade(controller);
        glib::timeout_add_local(Duration::from_millis(10), move || {
            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err(lg_buddy::tvs::TvsManagementError::stopped())
                }
            };
            if let Some(controller) = controller.upgrade() {
                let transition = controller
                    .application
                    .borrow_mut()
                    .complete_tvs_management(&operation, result);
                if let Some(transition) = transition {
                    Self::apply_transition(&controller, transition);
                }
            }
            drop(application_hold.take());
            glib::ControlFlow::Break
        });
    }

    fn start_pairing(controller: &Rc<Self>, operation: PairingOperation) {
        enum Update {
            Progress(PairingStage),
            Done(Result<lg_buddy::tvs::TvProfile, PairingError>),
            Stopped,
        }
        let backend = Arc::clone(&controller.pairing_backend);
        let worker_operation = operation.clone();
        let (sender, receiver) = mpsc::channel();
        // A started publication must finish even if the window closes.
        let mut application_hold = Some(controller.gtk_application.hold());
        thread::spawn(move || {
            let result = backend.pair(&worker_operation, &mut |stage| {
                let _ = sender.send(Update::Progress(stage));
            });
            let _ = sender.send(Update::Done(result));
        });
        let controller = Rc::downgrade(controller);
        glib::timeout_add_local(Duration::from_millis(10), move || loop {
            let update = match receiver.try_recv() {
                Ok(update) => update,
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => Update::Stopped,
            };
            let done = matches!(update, Update::Done(_) | Update::Stopped);
            if let Some(controller) = controller.upgrade() {
                let transition = match update {
                    Update::Progress(stage) => controller
                        .application
                        .borrow_mut()
                        .pairing_progress(&operation, stage),
                    Update::Done(result) => controller
                        .application
                        .borrow_mut()
                        .complete_pairing(&operation, result),
                    Update::Stopped => controller
                        .application
                        .borrow_mut()
                        .pairing_worker_stopped(&operation),
                };
                if let Some(transition) = transition {
                    Self::apply_transition(&controller, transition);
                }
            }
            if done {
                drop(application_hold.take());
                return glib::ControlFlow::Break;
            }
        });
    }

    fn start_tvs_model_read(controller: &Rc<Self>, operation: TvsModelReadOperation) {
        let backend = Arc::clone(&controller.tvs_backend);
        let profile = operation.profile().clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let _ = sender.send(backend.read_model_name(&profile));
        });
        let controller = Rc::downgrade(controller);
        glib::timeout_add_local(Duration::from_millis(10), move || {
            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => Err(TvsReadError::internal(
                    "the TV model operation stopped before returning a result",
                )),
            };
            if let Some(controller) = controller.upgrade() {
                let transition = controller
                    .application
                    .borrow_mut()
                    .complete_tvs_model_read(operation.clone(), result);
                if let Some(transition) = transition {
                    Self::apply_transition(&controller, transition);
                }
            }
            glib::ControlFlow::Break
        });
    }

    fn start_tvs_read(controller: &Rc<Self>, operation: TvsReadOperation) {
        let backend = Arc::clone(&controller.tvs_backend);
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let _ = sender.send(backend.read_profiles());
        });
        let controller = Rc::downgrade(controller);
        glib::timeout_add_local(Duration::from_millis(10), move || {
            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => Err(TvsReadError::internal(
                    "the TV profile operation stopped before returning a result",
                )),
            };
            if let Some(controller) = controller.upgrade() {
                let transition = controller
                    .application
                    .borrow_mut()
                    .complete_tvs_read(operation, result);
                if let Some(transition) = transition {
                    Self::apply_transition(&controller, transition);
                }
            }
            glib::ControlFlow::Break
        });
    }

    fn start_operation(controller: &Rc<Self>, operation: OverviewOperation) {
        let backend = Arc::clone(&controller.backend);
        let operation_for_error = operation;
        let (sender, receiver) = mpsc::sync_channel(1);
        let write_operation = matches!(
            operation,
            OverviewOperation::WriteBrightness(_) | OverviewOperation::WriteAudio(_)
        );
        let mut application_hold = write_operation.then(|| controller.gtk_application.hold());

        thread::spawn(move || {
            let result = match operation {
                OverviewOperation::ReadSummary(operation) => {
                    OverviewCompletion::Summary(operation, backend.read_summary())
                }
                OverviewOperation::ReadBrightness(operation) => {
                    OverviewCompletion::BrightnessRead(operation, backend.read_brightness())
                }
                OverviewOperation::ReadAudio(operation) => {
                    OverviewCompletion::AudioRead(operation, backend.read_audio())
                }
                OverviewOperation::WriteBrightness(operation) => {
                    OverviewCompletion::BrightnessWrite(
                        operation,
                        backend.write_brightness(operation.brightness()),
                    )
                }
                OverviewOperation::WriteAudio(operation) => OverviewCompletion::AudioWrite(
                    operation,
                    backend.write_audio(operation.operation()),
                ),
            };
            let _ = sender.send(result);
        });

        let controller = Rc::downgrade(controller);
        glib::timeout_add_local(Duration::from_millis(10), move || {
            match receiver.try_recv() {
                Ok(result) => {
                    if let Some(controller) = controller.upgrade() {
                        Self::complete(&controller, result);
                    }
                    drop(application_hold.take());
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if let Some(controller) = controller.upgrade() {
                        Self::complete(&controller, Self::disconnected_result(operation_for_error));
                    }
                    drop(application_hold.take());
                    glib::ControlFlow::Break
                }
            }
        });
    }

    fn disconnected_result(operation: OverviewOperation) -> OverviewCompletion {
        let message = "the Overview operation stopped before returning a result";
        match operation {
            OverviewOperation::ReadSummary(operation) => OverviewCompletion::Summary(
                operation,
                Err(OverviewSummaryError::new(
                    lg_buddy::overview::OverviewSummaryFailure::Internal,
                    message,
                )),
            ),
            OverviewOperation::ReadBrightness(operation) => OverviewCompletion::BrightnessRead(
                operation,
                Err(BrightnessReadError::new(
                    BrightnessReadFailure::Internal,
                    message,
                )),
            ),
            OverviewOperation::ReadAudio(operation) => OverviewCompletion::AudioRead(
                operation,
                Err(lg_buddy::overview::AudioReadError::new(
                    lg_buddy::overview::AudioReadFailure::Internal,
                    message,
                )),
            ),
            OverviewOperation::WriteBrightness(operation) => OverviewCompletion::BrightnessWrite(
                operation,
                Err(BrightnessWriteError::new(
                    BrightnessWriteFailure::Internal,
                    message,
                )),
            ),
            OverviewOperation::WriteAudio(operation) => OverviewCompletion::AudioWrite(
                operation,
                Err(AudioWriteError::new(
                    AudioWriteFailure::Internal,
                    message.to_string(),
                    None,
                )),
            ),
        }
    }

    fn complete(controller: &Rc<Self>, result: OverviewCompletion) {
        let transition = controller
            .application
            .borrow_mut()
            .complete_overview(result);
        if let Some(transition) = transition {
            Self::apply_transition(controller, transition);
        }
    }

    fn shutdown(&self) {
        self.application.borrow_mut().shutdown();
    }
}

fn connect_application(
    application: &adw::Application,
    controller: Rc<RefCell<Option<Rc<ApplicationController>>>>,
    backend: Arc<dyn OverviewBackend>,
    tvs_backend: Arc<dyn TvsBackend>,
) {
    application.connect_activate({
        let controller = Rc::clone(&controller);
        let backend = Arc::clone(&backend);
        let tvs_backend = Arc::clone(&tvs_backend);
        move |application| {
            if let Some(controller) = controller.borrow().as_ref() {
                controller.present();
                return;
            }
            let (overview, opening) = ApplicationController::new(
                application,
                Arc::clone(&backend),
                Arc::clone(&tvs_backend),
            );
            controller.replace(Some(Rc::clone(&overview)));
            ApplicationController::apply_transition(&overview, opening);
            overview.present();
        }
    });
    application.connect_shutdown(move |_| {
        if let Some(controller) = controller.borrow().as_ref() {
            controller.shutdown();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{help, parse_args, GuiCommand, GuiParseError};

    #[test]
    fn parses_the_brightness_command() {
        assert_eq!(parse_args(["brightness"]), Ok(GuiCommand::Brightness));
        assert_eq!(parse_args(["--version"]), Ok(GuiCommand::Version));
        assert_eq!(parse_args(["-V"]), Ok(GuiCommand::Version));
        assert_eq!(
            parse_args(std::iter::empty::<&str>()),
            Err(GuiParseError::MissingCommand)
        );
        assert_eq!(
            parse_args(["settings"]),
            Err(GuiParseError::UnknownCommand("settings".to_string()))
        );
        assert_eq!(
            parse_args(["brightness", "extra"]),
            Err(GuiParseError::UnexpectedArguments(
                vec!["extra".to_string()]
            ))
        );
        assert_eq!(
            help("lg-buddy-gui"),
            "Usage: lg-buddy-gui brightness\n       lg-buddy-gui --version\n"
        );
    }
}

#[cfg(test)]
pub(crate) mod controller_test_support {
    use std::cell::Cell;
    use std::net::Ipv4Addr;
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use gtk::glib;
    use gtk::prelude::*;
    use lg_buddy::audio::{AudioWriteError, AudioWriteFailure, AudioWriteOutcome};
    use lg_buddy::brightness::{BrightnessReadError, BrightnessWriteError, BrightnessWriteOutcome};
    use lg_buddy::config::{HdmiInput, TvPlatform};
    use lg_buddy::overview::{
        AudioReadError, OverviewBackend, OverviewIntent, OverviewOperation, OverviewSummaryError,
        OverviewTvIdentity,
    };
    use lg_buddy::tv::{AudioStatus, CurrentVolume, OledBrightness, VolumeLevel};

    use super::{ApplicationController, APPLICATION_ID};

    struct EmptyTvsBackend;
    impl lg_buddy::tvs::TvsBackend for EmptyTvsBackend {
        fn read_profiles(
            &self,
        ) -> Result<Vec<lg_buddy::tvs::TvProfile>, lg_buddy::tvs::TvsReadError> {
            Ok(Vec::new())
        }

        fn read_model_name(
            &self,
            _: &lg_buddy::tvs::TvProfile,
        ) -> Result<String, lg_buddy::tvs::TvsReadError> {
            panic!("an empty collection must not query a TV model")
        }
    }

    struct BlockingTvsBackend {
        profiles: Mutex<mpsc::Receiver<Vec<lg_buddy::tvs::TvProfile>>>,
        model: Mutex<mpsc::Receiver<String>>,
    }

    impl lg_buddy::tvs::TvsBackend for BlockingTvsBackend {
        fn read_profiles(
            &self,
        ) -> Result<Vec<lg_buddy::tvs::TvProfile>, lg_buddy::tvs::TvsReadError> {
            Ok(self.profiles.lock().unwrap().recv().unwrap())
        }

        fn read_model_name(
            &self,
            _: &lg_buddy::tvs::TvProfile,
        ) -> Result<String, lg_buddy::tvs::TvsReadError> {
            Ok(self.model.lock().unwrap().recv().unwrap())
        }
    }

    type SummaryResult = Result<OverviewTvIdentity, OverviewSummaryError>;
    type BrightnessResult = Result<OledBrightness, BrightnessReadError>;
    type AudioResult = Result<AudioStatus, AudioReadError>;
    type BrightnessWriteResult = Result<BrightnessWriteOutcome, BrightnessWriteError>;

    pub(crate) struct BackendControls {
        summary: mpsc::Sender<SummaryResult>,
        brightness: mpsc::Sender<BrightnessResult>,
        audio: mpsc::Sender<AudioResult>,
        brightness_write: mpsc::Sender<BrightnessWriteResult>,
        write_started: mpsc::Receiver<()>,
    }

    struct BlockingBackend {
        summary: Mutex<mpsc::Receiver<SummaryResult>>,
        brightness: Mutex<mpsc::Receiver<BrightnessResult>>,
        audio: Mutex<mpsc::Receiver<AudioResult>>,
        brightness_write: Mutex<mpsc::Receiver<BrightnessWriteResult>>,
        write_started: mpsc::Sender<()>,
    }

    impl BlockingBackend {
        fn new() -> (Self, BackendControls) {
            let (summary_tx, summary_rx) = mpsc::channel();
            let (brightness_tx, brightness_rx) = mpsc::channel();
            let (audio_tx, audio_rx) = mpsc::channel();
            let (brightness_write_tx, brightness_write_rx) = mpsc::channel();
            let (write_started_tx, write_started_rx) = mpsc::channel();
            (
                Self {
                    summary: Mutex::new(summary_rx),
                    brightness: Mutex::new(brightness_rx),
                    audio: Mutex::new(audio_rx),
                    brightness_write: Mutex::new(brightness_write_rx),
                    write_started: write_started_tx,
                },
                BackendControls {
                    summary: summary_tx,
                    brightness: brightness_tx,
                    audio: audio_tx,
                    brightness_write: brightness_write_tx,
                    write_started: write_started_rx,
                },
            )
        }
    }

    impl OverviewBackend for BlockingBackend {
        fn read_summary(&self) -> SummaryResult {
            self.summary
                .lock()
                .expect("summary receiver lock")
                .recv()
                .expect("summary test result")
        }

        fn read_brightness(&self) -> BrightnessResult {
            self.brightness
                .lock()
                .expect("brightness receiver lock")
                .recv()
                .expect("brightness test result")
        }

        fn read_audio(&self) -> AudioResult {
            self.audio
                .lock()
                .expect("audio receiver lock")
                .recv()
                .expect("audio test result")
        }

        fn write_brightness(&self, _brightness: OledBrightness) -> BrightnessWriteResult {
            self.write_started.send(()).expect("write start receiver");
            self.brightness_write
                .lock()
                .expect("brightness write receiver lock")
                .recv()
                .expect("brightness write test result")
        }

        fn write_audio(
            &self,
            _operation: lg_buddy::audio::AudioOperation,
        ) -> Result<AudioWriteOutcome, AudioWriteError> {
            Err(AudioWriteError::new(
                AudioWriteFailure::Internal,
                "audio write is not part of this controller scenario".to_string(),
                None,
            ))
        }
    }

    #[derive(Debug, Default)]
    struct PanicBackend;

    impl OverviewBackend for PanicBackend {
        fn read_summary(&self) -> SummaryResult {
            panic!("simulated summary worker panic")
        }

        fn read_brightness(&self) -> BrightnessResult {
            unreachable!("scenario starts only the summary operation")
        }

        fn read_audio(&self) -> AudioResult {
            unreachable!("scenario starts only the summary operation")
        }

        fn write_brightness(&self, _brightness: OledBrightness) -> BrightnessWriteResult {
            unreachable!("scenario does not write brightness")
        }

        fn write_audio(
            &self,
            _operation: lg_buddy::audio::AudioOperation,
        ) -> Result<AudioWriteOutcome, AudioWriteError> {
            unreachable!("scenario does not write audio")
        }
    }

    fn test_application(suffix: &str) -> adw::Application {
        let application = adw::Application::builder()
            .application_id(format!("{APPLICATION_ID}.Controller{suffix}"))
            .build();
        application
            .register(None::<&gtk::gio::Cancellable>)
            .expect("register controller application");
        application
    }

    fn pump_until(mut ready: impl FnMut() -> bool) {
        let context = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !ready() {
            while context.pending() {
                context.iteration(false);
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for GTK operation"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn pump_for(duration: Duration) {
        let context = glib::MainContext::default();
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            while context.pending() {
                context.iteration(false);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn widget_contains_text(widget: &gtk::Widget, expected: &str) -> bool {
        if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
            if label.label().contains(expected) {
                return true;
            }
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if widget_contains_text(&current, expected) {
                return true;
            }
            child = current.next_sibling();
        }
        false
    }

    fn scale_count(widget: &gtk::Widget) -> usize {
        let own =
            usize::from(widget.is_visible() && widget.clone().downcast::<gtk::Scale>().is_ok());
        let mut total = own;
        let mut child = widget.first_child();
        while let Some(current) = child {
            total += scale_count(&current);
            child = current.next_sibling();
        }
        total
    }

    fn find_operation(
        operations: &[OverviewOperation],
        matches: impl Fn(OverviewOperation) -> bool,
    ) -> OverviewOperation {
        operations
            .iter()
            .copied()
            .find(|operation| matches(*operation))
            .expect("expected Overview operation")
    }

    pub(crate) fn run_scenario() {
        assert!(
            gtk::is_initialized(),
            "renderer test must initialize GTK first"
        );
        run_pairing_scenario();

        let (backend, controls) = BlockingBackend::new();
        let application = test_application("Blocking");
        let (profiles_tx, profiles_rx) = mpsc::channel();
        let (model_tx, model_rx) = mpsc::channel();
        let (controller, opening) = ApplicationController::new(
            &application,
            Arc::new(backend),
            Arc::new(BlockingTvsBackend {
                profiles: Mutex::new(profiles_rx),
                model: Mutex::new(model_rx),
            }),
        );
        ApplicationController::apply_transition(&controller, opening.clone());
        let native_window = controller.window.window();
        controller.present();
        controller.present();
        assert_eq!(controller.window.window(), native_window);
        assert_eq!(application.windows().len(), 1);

        let heartbeat = std::rc::Rc::new(Cell::new(0usize));
        let heartbeat_source = glib::timeout_add_local(Duration::from_millis(5), {
            let heartbeat = std::rc::Rc::clone(&heartbeat);
            move || {
                heartbeat.set(heartbeat.get() + 1);
                glib::ControlFlow::Continue
            }
        });
        pump_until(|| heartbeat.get() >= 2);

        controls
            .summary
            .send(Ok(OverviewTvIdentity::new(
                Ipv4Addr::new(192, 0, 2, 1),
                HdmiInput::Hdmi1,
                TvPlatform::LgWebOs,
            )))
            .expect("summary result receiver");
        pump_until(|| widget_contains_text(&controller.window.window().upcast(), "192.0.2.1"));

        controller.window.choose_page(super::ApplicationPage::Tvs);
        pump_for(Duration::from_millis(30));
        assert_eq!(
            controller.navigation.borrow().selected(),
            super::ApplicationPage::Tvs
        );
        profiles_tx
            .send(vec![lg_buddy::tvs::TvProfile::new(
                "primary",
                "Primary TV",
                Ipv4Addr::new(192, 0, 2, 1),
                "aa:bb:cc:dd:ee:ff".parse().unwrap(),
                HdmiInput::Hdmi1,
                TvPlatform::LgWebOs,
                lg_buddy::tvs::TvCredentialState::Stored,
            )])
            .unwrap();
        pump_until(|| widget_contains_text(&native_window.clone().upcast(), "Primary TV"));
        let tvs_focus = gtk::prelude::GtkWindowExt::focus(&native_window);

        controls
            .brightness
            .send(Ok(OledBrightness::new(50).expect("valid brightness")))
            .expect("brightness result receiver");
        pump_until(|| scale_count(&controller.window.window().upcast()) == 1);

        controls
            .audio
            .send(Ok(AudioStatus::new(
                CurrentVolume::Level(VolumeLevel::new(20).expect("valid volume")),
                true,
            )))
            .expect("audio result receiver");
        pump_until(|| scale_count(&controller.window.window().upcast()) == 2);
        assert_eq!(
            controller.navigation.borrow().selected(),
            super::ApplicationPage::Tvs
        );
        assert_eq!(
            gtk::prelude::GtkWindowExt::focus(&native_window),
            tvs_focus,
            "background Overview reads must preserve focus on TVs"
        );
        controller
            .window
            .choose_page(super::ApplicationPage::Overview);
        model_tx.send("OLED42C2".to_string()).unwrap();
        pump_until(|| widget_contains_text(&native_window.clone().upcast(), "OLED42C2"));
        assert_eq!(
            controller.navigation.borrow().selected(),
            super::ApplicationPage::Overview,
            "a background model result must not change the active page"
        );
        assert!(
            heartbeat.get() >= 2,
            "GTK heartbeat must run while reads are pending"
        );

        ApplicationController::handle_intent(&controller, OverviewIntent::SetBrightness(55));
        pump_until(|| controls.write_started.try_recv().is_ok());
        controller.window.choose_page(super::ApplicationPage::Tvs);
        controller
            .window
            .choose_page(super::ApplicationPage::Overview);
        assert!(
            controls.write_started.try_recv().is_err(),
            "navigation must not submit another write"
        );

        let heartbeat_before_close = heartbeat.get();
        ApplicationController::handle_intent(&controller, OverviewIntent::Cancel);
        assert!(controller.closed.get(), "Cancel closes the view");
        controller.present();
        assert!(controller.closed.get(), "closed view must not reactivate");
        pump_for(Duration::from_millis(30));
        assert!(
            heartbeat.get() > heartbeat_before_close,
            "pending write keeps GTK alive"
        );

        controls
            .brightness_write
            .send(Ok(BrightnessWriteOutcome::applied()))
            .expect("write result receiver");
        pump_for(Duration::from_millis(40));
        assert!(
            controller.closed.get(),
            "late completion must not reopen view"
        );
        heartbeat_source.remove();

        let panic_application = test_application("Panic");
        let (panic_controller, panic_opening) = ApplicationController::new(
            &panic_application,
            Arc::new(PanicBackend),
            Arc::new(EmptyTvsBackend),
        );
        let panic_opening = panic_opening.overview().unwrap();
        let initial = match panic_opening.update() {
            super::OverviewFrontendUpdate::Present(presentation) => presentation.clone(),
            super::OverviewFrontendUpdate::Close => panic!("opening must present"),
        };
        panic_controller.window.render(&initial);
        let summary_operation = find_operation(panic_opening.operations(), |operation| {
            matches!(operation, OverviewOperation::ReadSummary(_))
        });
        ApplicationController::start_operation(&panic_controller, summary_operation);
        pump_until(|| {
            widget_contains_text(
                &panic_controller.window.window().upcast(),
                "LG Buddy could not load the primary TV.",
            )
        });
        panic_controller.window.close();
        assert_cancel_waits_for_write_in_application_loop();
    }

    fn run_pairing_scenario() {
        use lg_buddy::pairing::{
            PairingBackend, PairingError, PairingFailure, PairingIntent, PairingOperation,
            PairingStage,
        };
        use lg_buddy::tvs::{TvCredentialState, TvId, TvProfile, TvsIntent};
        struct PairingMock {
            release: Mutex<mpsc::Receiver<()>>,
            reject: bool,
            panic: bool,
        }
        impl PairingBackend for PairingMock {
            fn pair(
                &self,
                operation: &PairingOperation,
                progress: &mut dyn FnMut(PairingStage),
            ) -> Result<TvProfile, PairingError> {
                assert!(!gtk::is_initialized_main_thread());
                progress(PairingStage::WaitingForConfirmation);
                self.release.lock().unwrap().recv().unwrap();
                if operation.is_cancelled() {
                    return Err(PairingError::new(PairingFailure::Cancelled));
                }
                assert!(!self.panic, "injected pairing worker failure");
                if self.reject {
                    return Err(PairingError::new(PairingFailure::Rejected));
                }
                progress(PairingStage::Verifying);
                let request = operation.request();
                Ok(TvProfile::new(
                    TvId::primary(),
                    "Primary TV",
                    request.address(),
                    request.mac(),
                    request.input(),
                    TvPlatform::LgWebOs,
                    TvCredentialState::Stored,
                ))
            }
        }
        struct TvsMock;
        impl lg_buddy::tvs::TvsBackend for TvsMock {
            fn read_profiles(&self) -> Result<Vec<TvProfile>, lg_buddy::tvs::TvsReadError> {
                Ok(vec![])
            }
            fn read_model_name(
                &self,
                _: &TvProfile,
            ) -> Result<String, lg_buddy::tvs::TvsReadError> {
                Ok("Test OLED".into())
            }
        }
        for (cancel, reject, panic, name) in [
            (false, false, false, "PairSuccess"),
            (true, false, false, "PairCancel"),
            (false, true, false, "PairRejected"),
            (false, false, true, "PairWorkerStopped"),
        ] {
            let application = test_application(name);
            let (backend, controls) = BlockingBackend::new();
            let (release, receiver) = mpsc::channel();
            let (controller, opening) = ApplicationController::with_pairing_backend(
                &application,
                Arc::new(backend),
                Arc::new(TvsMock),
                Arc::new(PairingMock {
                    release: Mutex::new(receiver),
                    reject,
                    panic,
                }),
            );
            ApplicationController::render_tvs_transition(&controller, opening.tvs().unwrap());
            controller.present();
            controller.navigate(lg_buddy::navigation::ApplicationPage::Tvs);
            pump_until(|| {
                widget_contains_text(controller.window.window().upcast_ref(), "No TV configured")
            });
            for intent in [
                TvsIntent::PairTv,
                TvsIntent::Pairing(PairingIntent::SetAddress("192.0.2.10".into())),
                TvsIntent::Pairing(PairingIntent::SetMac("02:11:22:33:44:55".into())),
                TvsIntent::Pairing(PairingIntent::Submit),
            ] {
                ApplicationController::handle_tvs_intent(&controller, intent);
            }
            pump_until(|| {
                widget_contains_text(
                    controller.window.window().upcast_ref(),
                    "Confirm on Your TV",
                )
            });
            assert!(!widget_contains_text(
                controller.window.window().upcast_ref(),
                "TV paired successfully",
            ));
            if cancel {
                assert!(controller.window.dismiss_dialog());
                assert!(!controller.closed.get());
                release.send(()).unwrap();
                pump_for(Duration::from_millis(60));
                assert!(!controller.application.borrow().is_pairing());
            } else if reject || panic {
                release.send(()).unwrap();
                pump_until(|| {
                    widget_contains_text(
                        controller.window.window().upcast_ref(),
                        "Could Not Pair TV",
                    )
                });
                assert!(controller.application.borrow().is_pairing());
                if panic {
                    assert!(widget_contains_text(
                        controller.window.window().upcast_ref(),
                        "Pairing stopped unexpectedly",
                    ));
                    assert!(!widget_contains_text(
                        controller.window.window().upcast_ref(),
                        "Check the IP address",
                    ));
                }
            } else {
                release.send(()).unwrap();
                controls
                    .summary
                    .send(Ok(OverviewTvIdentity::new(
                        "192.0.2.10".parse().unwrap(),
                        HdmiInput::Hdmi1,
                        TvPlatform::LgWebOs,
                    )))
                    .unwrap();
                controls
                    .brightness
                    .send(Ok(OledBrightness::new(50).unwrap()))
                    .unwrap();
                controls
                    .audio
                    .send(Ok(AudioStatus::new(
                        CurrentVolume::Level(VolumeLevel::new(25).unwrap()),
                        false,
                    )))
                    .unwrap();
                pump_until(|| {
                    widget_contains_text(controller.window.window().upcast_ref(), "Test OLED")
                });
                assert!(!controller.application.borrow().is_pairing());
                assert_eq!(
                    controller.navigation.borrow().selected(),
                    lg_buddy::navigation::ApplicationPage::Tvs
                );
            }
            assert_eq!(
                widget_contains_text(
                    controller.window.window().upcast_ref(),
                    "TV paired successfully",
                ),
                !cancel && !reject && !panic,
                "only successful pairing should show the confirmation toast",
            );
            ApplicationController::handle_intent(&controller, OverviewIntent::Cancel);
            assert!(controller.closed.get());
        }
    }

    fn assert_cancel_waits_for_write_in_application_loop() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let (backend, controls) = BlockingBackend::new();
        let application = test_application("WriteHold");
        let controller = Rc::new(RefCell::new(None));
        super::install_application_actions(&application, Rc::clone(&controller));
        super::connect_application(
            &application,
            Rc::clone(&controller),
            Arc::new(backend),
            Arc::new(EmptyTvsBackend),
        );
        controls
            .summary
            .send(Ok(OverviewTvIdentity::new(
                Ipv4Addr::LOCALHOST,
                HdmiInput::Hdmi1,
                TvPlatform::Bscpylgtv,
            )))
            .unwrap();
        controls
            .brightness
            .send(Ok(OledBrightness::new(50).unwrap()))
            .unwrap();
        controls
            .audio
            .send(Ok(AudioStatus::new(CurrentVolume::Unknown, false)))
            .unwrap();

        let controls = Rc::new(controls);
        let completion_sent = Rc::new(Cell::new(false));
        let activations = Rc::new(Cell::new(0));
        application.connect_activate({
            let controller = Rc::clone(&controller);
            let controls = Rc::clone(&controls);
            let completion_sent = Rc::clone(&completion_sent);
            let activations = Rc::clone(&activations);
            move |application| {
                activations.set(activations.get() + 1);
                if activations.get() > 1 {
                    return;
                }
                assert_eq!(application.windows().len(), 1);
                let application = application.clone();
                let controller = Rc::clone(&controller);
                let controls = Rc::clone(&controls);
                let completion_sent = Rc::clone(&completion_sent);
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut issued = false;
                glib::timeout_add_local(Duration::from_millis(5), move || {
                    assert!(Instant::now() < deadline, "write did not start");
                    let controller = controller.borrow().as_ref().unwrap().clone();
                    if !issued && scale_count(&controller.window.window().upcast()) == 1 {
                        ApplicationController::handle_intent(
                            &controller,
                            OverviewIntent::SetBrightness(55),
                        );
                        issued = true;
                    }
                    if controls.write_started.try_recv().is_err() {
                        return glib::ControlFlow::Continue;
                    }
                    application.activate();
                    assert_eq!(application.windows().len(), 1);
                    application.activate_action("quit", None);
                    assert!(application.windows().is_empty());
                    application.activate();
                    assert!(
                        application.windows().is_empty(),
                        "closed view must not reopen while write settles"
                    );
                    let controls = Rc::clone(&controls);
                    let completion_sent = Rc::clone(&completion_sent);
                    glib::timeout_add_local_once(Duration::from_millis(30), move || {
                        completion_sent.set(true);
                        controls
                            .brightness_write
                            .send(Ok(BrightnessWriteOutcome::Applied))
                            .unwrap();
                    });
                    glib::ControlFlow::Break
                });
            }
        });
        assert_eq!(
            application.run_with_args(&["overview-write-hold-test"]),
            glib::ExitCode::SUCCESS
        );
        assert!(
            completion_sent.get(),
            "application exited before the dispatched write settled"
        );
        assert_eq!(activations.get(), 3);
        assert!(application.windows().is_empty());
    }
}
