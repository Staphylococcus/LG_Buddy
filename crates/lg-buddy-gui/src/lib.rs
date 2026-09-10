mod diagnostics;
mod overview;
mod pairing;
mod settings;
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
use lg_buddy::diagnostics_view::{
    DiagnosticsBackend, DiagnosticsError, DiagnosticsIntent, DiagnosticsReadOperation,
    DiagnosticsSaveOperation, DiagnosticsSaveRequest, DiagnosticsTransition,
    EnvironmentDiagnosticsBackend,
};
use lg_buddy::navigation::ApplicationPage;
use lg_buddy::overview::{
    EnvironmentOverviewBackend, OverviewBackend, OverviewFrontendUpdate, OverviewIntent,
    OverviewOperation, OverviewSummaryError, OverviewTransition,
};
use lg_buddy::pairing::{
    EnvironmentPairingBackend, PairingBackend, PairingError, PairingOperation, PairingStage,
};
use lg_buddy::settings_view::{
    EnvironmentSettingsBackend, SettingsBackend, SettingsIntent, SettingsMutationOperation,
    SettingsReadError, SettingsReadOperation, SettingsTransition, UpdateCheckError,
    UpdateCheckOperation,
};
use lg_buddy::tvs::{
    EnvironmentTvsBackend, TvsBackend, TvsIntent, TvsModelReadOperation, TvsReadError,
    TvsReadOperation, TvsTransition,
};
use lg_buddy::update_flow::{
    EnvironmentUpdateInstallBackend, UpdateInstallBackend, UpdateInstallFailure,
    UpdateInstallOperation, UpdateInstallOutcome, UpdateInstallTask,
};

pub const APPLICATION_ID: &str = "io.github.staphylococcus.LGBuddy";
pub const APPLICATION_NAME: &str = "LG Buddy";

fn register_resources() {
    static RESOURCES: std::sync::Once = std::sync::Once::new();
    RESOURCES.call_once(|| {
        gtk::gio::resources_register_include!("lg-buddy-gui.gresource")
            .expect("bundled GUI resources must be valid");
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiCommand {
    Overview,
    Brightness,
    Version,
    /// Internal entry point used only by the post-install process handoff.
    Relaunch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuiParseError {
    UnknownCommand(String),
    UnexpectedArguments(Vec<String>),
}

impl fmt::Display for GuiParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
        Some(command) if command.as_ref() == "--gapplication-replace" => GuiCommand::Relaunch,
        Some(command) if command.as_ref() == "brightness" => GuiCommand::Brightness,
        Some(command) if matches!(command.as_ref(), "--version" | "-V") => GuiCommand::Version,
        Some(command) => return Err(GuiParseError::UnknownCommand(command.as_ref().to_string())),
        None => GuiCommand::Overview,
    };
    let unexpected: Vec<String> = args.map(|argument| argument.as_ref().to_string()).collect();
    if !unexpected.is_empty() {
        return Err(GuiParseError::UnexpectedArguments(unexpected));
    }
    Ok(command)
}

pub fn help(program: &str) -> String {
    format!("Usage: {program} [brightness]\n       {program} --version\n")
}

pub fn run(command: GuiCommand) -> glib::ExitCode {
    match command {
        GuiCommand::Overview | GuiCommand::Brightness => run_application(command, false),
        GuiCommand::Relaunch => run_application(GuiCommand::Overview, true),
        GuiCommand::Version => {
            print!("{}", lg_buddy::version::version_text());
            glib::ExitCode::SUCCESS
        }
    }
}

fn run_application(command: GuiCommand, replacing: bool) -> glib::ExitCode {
    glib::set_application_name(APPLICATION_NAME);
    let mut flags = gtk::gio::ApplicationFlags::HANDLES_COMMAND_LINE
        | gtk::gio::ApplicationFlags::ALLOW_REPLACEMENT;
    if replacing {
        flags |= gtk::gio::ApplicationFlags::REPLACE;
    }
    let application = adw::Application::builder()
        .application_id(APPLICATION_ID)
        .flags(flags)
        .build();
    let controller = Rc::new(RefCell::new(None::<Rc<ApplicationController>>));
    install_application_actions(&application, Rc::clone(&controller));
    connect_application(
        &application,
        Rc::clone(&controller),
        Arc::new(EnvironmentOverviewBackend),
        Arc::new(EnvironmentTvsBackend),
        Arc::new(EnvironmentSettingsBackend),
    );
    let arguments: &[&str] = match (command, replacing) {
        (_, true) => &["lg-buddy-gui", "--gapplication-replace"],
        (GuiCommand::Brightness, false) => &["lg-buddy-gui", "brightness"],
        _ => &["lg-buddy-gui"],
    };
    application.run_with_args(arguments)
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
    settings_backend: Arc<dyn SettingsBackend>,
    update_install_backend: Arc<dyn UpdateInstallBackend>,
    diagnostics_backend: Arc<dyn DiagnosticsBackend>,
    backend: Arc<dyn OverviewBackend>,
    closed: Cell<bool>,
}

impl ApplicationController {
    fn new(
        gtk_application: &adw::Application,
        backend: Arc<dyn OverviewBackend>,
        tvs_backend: Arc<dyn TvsBackend>,
        settings_backend: Arc<dyn SettingsBackend>,
    ) -> (Rc<Self>, ApplicationTransition) {
        Self::with_backends(
            gtk_application,
            backend,
            tvs_backend,
            Arc::new(EnvironmentPairingBackend),
            settings_backend,
        )
    }

    fn with_backends(
        gtk_application: &adw::Application,
        backend: Arc<dyn OverviewBackend>,
        tvs_backend: Arc<dyn TvsBackend>,
        pairing_backend: Arc<dyn PairingBackend>,
        settings_backend: Arc<dyn SettingsBackend>,
    ) -> (Rc<Self>, ApplicationTransition) {
        Self::with_update_backend(
            gtk_application,
            backend,
            tvs_backend,
            pairing_backend,
            settings_backend,
            Arc::new(EnvironmentUpdateInstallBackend),
        )
    }

    fn with_update_backend(
        gtk_application: &adw::Application,
        backend: Arc<dyn OverviewBackend>,
        tvs_backend: Arc<dyn TvsBackend>,
        pairing_backend: Arc<dyn PairingBackend>,
        settings_backend: Arc<dyn SettingsBackend>,
        update_install_backend: Arc<dyn UpdateInstallBackend>,
    ) -> (Rc<Self>, ApplicationTransition) {
        Self::with_all_backends(
            gtk_application,
            backend,
            tvs_backend,
            pairing_backend,
            settings_backend,
            update_install_backend,
            Arc::new(EnvironmentDiagnosticsBackend),
        )
    }

    fn with_all_backends(
        gtk_application: &adw::Application,
        backend: Arc<dyn OverviewBackend>,
        tvs_backend: Arc<dyn TvsBackend>,
        pairing_backend: Arc<dyn PairingBackend>,
        settings_backend: Arc<dyn SettingsBackend>,
        update_install_backend: Arc<dyn UpdateInstallBackend>,
        diagnostics_backend: Arc<dyn DiagnosticsBackend>,
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
                        Self::navigate(&controller, page);
                    }
                }
            });
            let on_settings = Rc::new({
                let controller = controller.clone();
                move |intent| {
                    if let Some(controller) = controller.upgrade() {
                        Self::handle_settings_intent(&controller, intent);
                    }
                }
            });
            let on_diagnostics = Rc::new({
                let controller = controller.clone();
                move |intent| {
                    if let Some(controller) = controller.upgrade() {
                        Self::handle_diagnostics_intent(&controller, intent);
                    }
                }
            });
            Self {
                tvs_backend,
                pairing_backend,
                settings_backend,
                update_install_backend,
                diagnostics_backend,
                application: RefCell::new(application),
                gtk_application: gtk_application.clone(),
                window: window::ApplicationWindow::new(
                    gtk_application,
                    on_intent,
                    on_tvs,
                    on_settings,
                    on_navigation,
                    on_diagnostics,
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
        controller
            .window
            .set_navigation_visible(transition.navigation().tabs_visible());
        controller
            .window
            .navigate(transition.navigation().selected());
        if let Some(settings) = transition.settings() {
            Self::render_settings_transition(controller, settings);
        }
        if let Some(tvs) = transition.tvs() {
            Self::render_tvs_transition(controller, tvs);
        }
        if let Some(overview) = transition.overview() {
            Self::render_overview_transition(controller, overview);
        }
        if let Some(diagnostics) = transition.diagnostics() {
            Self::render_diagnostics_transition(controller, diagnostics);
        }
    }

    fn handle_diagnostics_intent(controller: &Rc<Self>, intent: DiagnosticsIntent) {
        let transition = controller
            .application
            .borrow_mut()
            .handle_diagnostics_intent(intent);
        if let Some(transition) = transition {
            Self::apply_transition(controller, transition);
        }
    }

    fn render_diagnostics_transition(controller: &Rc<Self>, transition: &DiagnosticsTransition) {
        if controller.closed.get() {
            return;
        }
        controller
            .window
            .render_diagnostics(transition.presentation());
        if let Some(text) = transition.clipboard_text() {
            controller.window.window().clipboard().set_text(text);
        }
        if let Some(message) = transition.toast() {
            controller.window.show_toast(message);
        }
        if let Some(operation) = transition.read_operation() {
            Self::start_diagnostics_read(controller, operation.clone());
        }
        if let Some(request) = transition.save_request() {
            Self::choose_diagnostics_destination(controller, request);
        }
        if let Some(operation) = transition.save_operation() {
            Self::start_diagnostics_save(controller, operation.clone());
        }
    }

    fn start_diagnostics_read(controller: &Rc<Self>, operation: DiagnosticsReadOperation) {
        let backend = Arc::clone(&controller.diagnostics_backend);
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let _ = sender.send(backend.collect());
        });
        let controller = Rc::downgrade(controller);
        glib::timeout_add_local(Duration::from_millis(10), move || {
            let Some(controller) = controller.upgrade().filter(|value| !value.closed.get()) else {
                return glib::ControlFlow::Break;
            };
            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err(DiagnosticsError::collection_stopped())
                }
            };
            let transition = controller
                .application
                .borrow_mut()
                .complete_diagnostics_read(&operation, result);
            if let Some(transition) = transition {
                Self::apply_transition(&controller, transition);
            }
            glib::ControlFlow::Break
        });
    }

    fn choose_diagnostics_destination(controller: &Rc<Self>, request: DiagnosticsSaveRequest) {
        let dialog = gtk::FileDialog::builder()
            .title("Save Diagnostic Report")
            .initial_name("lg-buddy-diagnostics.txt")
            .modal(true)
            .build();
        let weak_controller = Rc::downgrade(controller);
        dialog.save(
            Some(&controller.window.window()),
            None::<&gtk::gio::Cancellable>,
            move |result| {
                let Some(controller) = weak_controller.upgrade() else {
                    return;
                };
                let intent = match result {
                    Ok(file) => match file.path() {
                        Some(path) => DiagnosticsIntent::SaveDestination {
                            request,
                            path: Some(path),
                        },
                        None => DiagnosticsIntent::SaveSelectionFailed(request),
                    },
                    Err(error)
                        if error.matches(gtk::DialogError::Dismissed)
                            || error.matches(gtk::DialogError::Cancelled) =>
                    {
                        DiagnosticsIntent::SaveDestination {
                            request,
                            path: None,
                        }
                    }
                    Err(_) => DiagnosticsIntent::SaveSelectionFailed(request),
                };
                Self::handle_diagnostics_intent(&controller, intent);
            },
        );
    }

    fn start_diagnostics_save(controller: &Rc<Self>, operation: DiagnosticsSaveOperation) {
        let backend = Arc::clone(&controller.diagnostics_backend);
        let worker_operation = operation.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        // An accepted export finishes even if its dialog or window closes.
        let application_hold = controller.gtk_application.hold();
        thread::spawn(move || {
            let _ = sender.send(backend.save(&worker_operation));
        });
        let controller = Rc::clone(controller);
        glib::timeout_add_local(Duration::from_millis(10), move || {
            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => Err(DiagnosticsError::save_failed()),
            };
            let transition = controller
                .application
                .borrow_mut()
                .complete_diagnostics_save(&operation, result);
            if let Some(transition) = transition {
                Self::apply_transition(&controller, transition);
            }
            let _ = &application_hold;
            glib::ControlFlow::Break
        });
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

    fn navigate(controller: &Rc<Self>, page: ApplicationPage) {
        if !controller.closed.get() {
            let transition = controller.application.borrow_mut().select_page(page);
            if let Some(transition) = transition {
                Self::apply_transition(controller, transition);
            }
        }
    }

    fn handle_settings_intent(controller: &Rc<Self>, intent: SettingsIntent) {
        let transition = controller
            .application
            .borrow_mut()
            .handle_settings_intent(intent);
        if let Some(transition) = transition {
            Self::apply_transition(controller, transition);
        }
    }

    fn render_settings_transition(controller: &Rc<Self>, transition: &SettingsTransition) {
        if let Some(diagnostic) = transition.diagnostic() {
            eprintln!("LG Buddy GUI: {diagnostic}");
        }
        if !controller.closed.get() {
            controller.window.render_settings(transition.presentation());
            if let Some(notice) = transition.update_notice() {
                controller.window.show_update_notice(notice);
            }
        }
        if let Some(operation) = transition.read_operation() {
            Self::start_settings_read(controller, operation);
        }
        if let Some(operation) = transition.mutation_operation() {
            Self::start_settings_mutation(controller, operation.clone());
        }
        if let Some(operation) = transition.update_check_operation() {
            Self::start_update_check(controller, operation);
        }
        if let Some(operation) = transition.update_install_operation() {
            Self::start_update_install(controller, operation.clone());
        }
    }

    fn start_update_install(controller: &Rc<Self>, operation: UpdateInstallOperation) {
        if let UpdateInstallTask::Relaunch(installed) = operation.task() {
            let result = controller.update_install_backend.relaunch(installed);
            let relaunched = result.is_ok();
            let transition = controller.application.borrow_mut().complete_update_install(
                &operation,
                result.map(|_| UpdateInstallOutcome::Relaunched),
            );
            if let Some(transition) = transition {
                Self::apply_transition(controller, transition);
            }
            if relaunched {
                controller.gtk_application.quit();
            }
            return;
        }
        enum Update {
            Progress(lg_buddy::update_install::UpdateInstallStage),
            Done(Box<Result<UpdateInstallOutcome, UpdateInstallFailure>>),
        }
        let backend = Arc::clone(&controller.update_install_backend);
        let worker_operation = operation.clone();
        let (sender, receiver) = mpsc::channel();
        // A cancelled download must settle and release its bundle lock; once
        // installation starts the application remains open until completion.
        let application_hold = controller.gtk_application.hold();
        thread::spawn(move || {
            let result = backend.run(&worker_operation, &mut |stage| {
                let _ = sender.send(Update::Progress(stage));
            });
            let _ = sender.send(Update::Done(Box::new(result)));
        });
        let controller = Rc::clone(controller);
        glib::timeout_add_local(Duration::from_millis(10), move || loop {
            let update = match receiver.try_recv() {
                Ok(update) => update,
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => Update::Done(Box::new(Err(
                    UpdateInstallFailure::worker_stopped(&operation),
                ))),
            };
            let done = matches!(update, Update::Done(_));
            let transition = match update {
                Update::Progress(stage) => controller
                    .application
                    .borrow_mut()
                    .update_install_progress(&operation, stage),
                Update::Done(result) => controller
                    .application
                    .borrow_mut()
                    .complete_update_install(&operation, *result),
            };
            if let Some(transition) = transition {
                Self::apply_transition(&controller, transition);
            }
            if done {
                let _ = &application_hold;
                return glib::ControlFlow::Break;
            }
        });
    }

    fn start_update_check(controller: &Rc<Self>, operation: UpdateCheckOperation) {
        let backend = Arc::clone(&controller.settings_backend);
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let _ = sender.send(backend.check_for_updates());
        });
        // A read must not keep the application alive after its window closes.
        let controller = Rc::downgrade(controller);
        glib::timeout_add_local(Duration::from_millis(10), move || {
            let Some(controller) = controller.upgrade().filter(|value| !value.closed.get()) else {
                return glib::ControlFlow::Break;
            };
            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => Err(UpdateCheckError::stopped()),
            };
            let transition = controller
                .application
                .borrow_mut()
                .complete_update_check(operation, result);
            if let Some(transition) = transition {
                Self::apply_transition(&controller, transition);
            }
            glib::ControlFlow::Break
        });
    }

    fn start_settings_mutation(controller: &Rc<Self>, operation: SettingsMutationOperation) {
        let backend = Arc::clone(&controller.settings_backend);
        let worker_operation = operation.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        // Keep the controller and application alive until accepted writes drain.
        let application_hold = controller.gtk_application.hold();
        thread::spawn(move || {
            let result = backend.write_setting(worker_operation, &mut |_| {});
            let _ = sender.send(result);
        });
        let controller = Rc::clone(controller);
        glib::timeout_add_local(Duration::from_millis(10), move || {
            let transition = match receiver.try_recv() {
                Ok(result) => controller
                    .application
                    .borrow_mut()
                    .complete_settings_mutation(&operation, result),
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => controller
                    .application
                    .borrow_mut()
                    .settings_mutation_worker_stopped(&operation),
            };
            if let Some(transition) = transition {
                Self::apply_transition(&controller, transition);
            }
            let _ = &application_hold;
            glib::ControlFlow::Break
        });
    }

    fn start_settings_read(controller: &Rc<Self>, operation: SettingsReadOperation) {
        let backend = Arc::clone(&controller.settings_backend);
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let _ = sender.send(backend.read_settings());
        });
        let controller = Rc::clone(controller);
        let application_hold = controller.gtk_application.hold();
        glib::timeout_add_local(Duration::from_millis(10), move || {
            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => Err(SettingsReadError::stopped()),
            };
            let transition = controller
                .application
                .borrow_mut()
                .complete_settings_read(operation, result);
            if let Some(transition) = transition {
                Self::apply_transition(&controller, transition);
            }
            let _ = &application_hold;
            glib::ControlFlow::Break
        });
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
        if !controller.closed.get() {
            controller.window.render_tvs(transition.presentation());
            if let Some(message) = transition.toast_message() {
                controller.window.show_toast(message);
            }
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
        let application_hold = controller.gtk_application.hold();
        thread::spawn(move || {
            let _ = sender.send(backend.manage(&worker_operation));
        });
        let controller = Rc::clone(controller);
        glib::timeout_add_local(Duration::from_millis(10), move || {
            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err(lg_buddy::tvs::TvsManagementError::stopped())
                }
            };
            let transition = controller
                .application
                .borrow_mut()
                .complete_tvs_management(&operation, result);
            if let Some(transition) = transition {
                Self::apply_transition(&controller, transition);
            }
            let _ = &application_hold;
            glib::ControlFlow::Break
        });
    }

    fn start_pairing(controller: &Rc<Self>, operation: PairingOperation) {
        enum Update {
            Progress(PairingStage),
            Done(Result<lg_buddy::pairing::PairingOutcome, PairingError>),
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
    settings_backend: Arc<dyn SettingsBackend>,
) {
    application.connect_activate({
        let controller = Rc::clone(&controller);
        let backend = Arc::clone(&backend);
        let tvs_backend = Arc::clone(&tvs_backend);
        let settings_backend = Arc::clone(&settings_backend);
        move |application| {
            if let Some(controller) = controller.borrow().as_ref() {
                ApplicationController::navigate(controller, ApplicationPage::Overview);
                controller.present();
                return;
            }
            let (overview, opening) = ApplicationController::new(
                application,
                Arc::clone(&backend),
                Arc::clone(&tvs_backend),
                Arc::clone(&settings_backend),
            );
            controller.replace(Some(Rc::clone(&overview)));
            ApplicationController::apply_transition(&overview, opening);
            overview.present();
        }
    });
    // GApplication forwards the request to the existing instance as well.
    application.connect_command_line({
        let controller = Rc::clone(&controller);
        move |application, command_line| {
            let arguments = command_line.arguments();
            let command =
                match parse_args(arguments.iter().skip(1).map(|arg| arg.to_string_lossy())) {
                    Ok(command @ (GuiCommand::Overview | GuiCommand::Brightness)) => command,
                    _ => return glib::ExitCode::FAILURE,
                };
            application.activate();
            if command == GuiCommand::Brightness {
                if let Some(controller) = controller.borrow().as_ref() {
                    if !controller.closed.get() {
                        controller.window.focus_brightness();
                    }
                }
            }
            glib::ExitCode::SUCCESS
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
    fn parses_overview_and_brightness_entrypoints() {
        assert_eq!(parse_args(["brightness"]), Ok(GuiCommand::Brightness));
        assert_eq!(parse_args(["--version"]), Ok(GuiCommand::Version));
        assert_eq!(parse_args(["-V"]), Ok(GuiCommand::Version));
        assert_eq!(
            parse_args(["--gapplication-replace"]),
            Ok(GuiCommand::Relaunch)
        );
        assert_eq!(
            parse_args(["--gapplication-replace", "brightness"]),
            Err(GuiParseError::UnexpectedArguments(vec![
                "brightness".to_string()
            ]))
        );
        assert_eq!(
            parse_args(std::iter::empty::<&str>()),
            Ok(GuiCommand::Overview)
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
            "Usage: lg-buddy-gui [brightness]\n       lg-buddy-gui --version\n"
        );
    }
}

#[cfg(test)]
pub(crate) mod controller_test_support {
    use std::cell::Cell;
    use std::net::Ipv4Addr;
    use std::rc::Rc;
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use adw::prelude::PreferencesRowExt;
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

    use super::{ApplicationController, ApplicationTransition, APPLICATION_ID};

    struct EmptyTvsBackend;
    struct DefaultSettingsBackend;

    impl lg_buddy::settings_view::SettingsBackend for DefaultSettingsBackend {
        fn check_for_updates(
            &self,
        ) -> Result<
            lg_buddy::presentation::update_check::UpdateCheckReport,
            lg_buddy::settings_view::UpdateCheckError,
        > {
            panic!("unexpected update check")
        }

        fn write_setting(
            &self,
            _operation: lg_buddy::settings_view::SettingsMutationOperation,
            _progress: &mut dyn FnMut(lg_buddy::settings::SettingsMutationStage),
        ) -> Result<
            lg_buddy::settings::SettingsMutationOutcome,
            lg_buddy::settings::SettingsMutationFailure,
        > {
            panic!("unexpected write in a read-only test backend")
        }

        fn read_settings(
            &self,
        ) -> Result<
            Vec<lg_buddy::presentation::settings::SettingsGroup>,
            lg_buddy::settings_view::SettingsReadError,
        > {
            let store = lg_buddy::settings::ConfigEnvReader::parse("/unused/config.env", "");
            Ok(
                lg_buddy::presentation::settings::SettingsPresentation::from_store(
                    &lg_buddy::settings::SettingsStore::from_reader(store),
                )
                .groups()
                .to_vec(),
            )
        }
    }
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

    fn configure_application_navigation(
        controller: &Rc<ApplicationController>,
        opening: &ApplicationTransition,
    ) {
        let operation = opening
            .tvs()
            .expect("opening TVs transition")
            .read_operation()
            .expect("opening TVs read");
        let profile = lg_buddy::tvs::TvProfile::new(
            lg_buddy::tvs::TvId::primary(),
            "Primary TV",
            Ipv4Addr::new(192, 0, 2, 10),
            "02:11:22:33:44:55".parse().unwrap(),
            HdmiInput::Hdmi1,
            TvPlatform::LgWebOs,
            lg_buddy::tvs::TvCredentialState::Stored,
        );
        let transition = controller
            .application
            .borrow_mut()
            .complete_tvs_read(operation, Ok(vec![profile]))
            .expect("configured TVs read");
        assert!(transition.navigation().tabs_visible());
        ApplicationController::render_settings_transition(
            controller,
            opening.settings().expect("opening Settings read"),
        );
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

    #[track_caller]
    pub(crate) fn pump_until(mut ready: impl FnMut() -> bool) {
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
        if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
            if entry.text() == expected {
                return true;
            }
        }
        if let Some(view) = widget.downcast_ref::<gtk::TextView>() {
            let buffer = view.buffer();
            if buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .contains(expected)
            {
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
        run_settings_scenario();
        run_settings_write_scenario();
        run_manual_update_check_scenario();
        run_update_install_scenario();
        run_diagnostics_scenario();

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
            Arc::new(DefaultSettingsBackend),
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

        ApplicationController::navigate(&controller, super::ApplicationPage::Tvs);
        pump_for(Duration::from_millis(30));
        assert_eq!(
            controller.window.visible_page(),
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
        ApplicationController::navigate(&controller, super::ApplicationPage::Tvs);
        pump_for(Duration::from_millis(30));
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
            controller.window.visible_page(),
            super::ApplicationPage::Tvs
        );
        assert_eq!(
            gtk::prelude::GtkWindowExt::focus(&native_window),
            tvs_focus,
            "background Overview reads must preserve focus on TVs"
        );
        ApplicationController::navigate(&controller, super::ApplicationPage::Overview);
        model_tx.send("OLED42C2".to_string()).unwrap();
        pump_until(|| widget_contains_text(&native_window.clone().upcast(), "OLED42C2"));
        assert_eq!(
            controller.window.visible_page(),
            super::ApplicationPage::Overview,
            "a background model result must not change the active page"
        );
        assert!(
            heartbeat.get() >= 2,
            "GTK heartbeat must run while reads are pending"
        );

        ApplicationController::handle_intent(&controller, OverviewIntent::SetBrightness(55));
        pump_until(|| controls.write_started.try_recv().is_ok());
        ApplicationController::navigate(&controller, super::ApplicationPage::Tvs);
        ApplicationController::navigate(&controller, super::ApplicationPage::Overview);
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
            Arc::new(DefaultSettingsBackend),
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

    fn run_settings_scenario() {
        use lg_buddy::presentation::settings::{SettingsGroup, SettingsPresentation};
        use lg_buddy::settings::ConfigEnvReader;
        use lg_buddy::settings_view::{SettingsBackend, SettingsReadError};

        struct SettingsMock(
            Mutex<std::collections::VecDeque<Result<Vec<SettingsGroup>, SettingsReadError>>>,
        );
        impl SettingsBackend for SettingsMock {
            fn check_for_updates(
                &self,
            ) -> Result<
                lg_buddy::presentation::update_check::UpdateCheckReport,
                lg_buddy::settings_view::UpdateCheckError,
            > {
                panic!("unexpected update check")
            }

            fn write_setting(
                &self,
                _operation: lg_buddy::settings_view::SettingsMutationOperation,
                _progress: &mut dyn FnMut(lg_buddy::settings::SettingsMutationStage),
            ) -> Result<
                lg_buddy::settings::SettingsMutationOutcome,
                lg_buddy::settings::SettingsMutationFailure,
            > {
                panic!("unexpected write in a read-only test backend")
            }

            fn read_settings(&self) -> Result<Vec<SettingsGroup>, SettingsReadError> {
                self.0
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("expected settings read")
            }
        }
        fn settings(timeout: u32) -> Vec<SettingsGroup> {
            let store = ConfigEnvReader::parse(
                "/unused/config.env",
                &format!("screen_idle_timeout={timeout}\n"),
            )
            .into_store();
            SettingsPresentation::from_store(&store).groups().to_vec()
        }
        fn retry_button(widget: &gtk::Widget) -> Option<gtk::Button> {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                if button.label().as_deref() == Some("Retry") && button.is_visible() {
                    return Some(button.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(button) = retry_button(&current) {
                    return Some(button);
                }
                child = current.next_sibling();
            }
            None
        }

        let application = test_application("Settings");
        let backend = std::sync::Arc::new(SettingsMock(Mutex::new(
            std::collections::VecDeque::from([
                Err(SettingsReadError::unreadable("test settings read failure")),
                Ok(settings(600)),
                Ok(settings(120)),
            ]),
        )));
        let (controller, opening) = ApplicationController::new(
            &application,
            Arc::new(PanicBackend),
            Arc::new(EmptyTvsBackend),
            backend,
        );
        configure_application_navigation(&controller, &opening);
        ApplicationController::navigate(&controller, super::ApplicationPage::Settings);
        controller.present();
        let native = controller.window.window();
        pump_until(|| {
            widget_contains_text(native.upcast_ref(), "LG Buddy could not read its settings")
        });
        retry_button(native.upcast_ref())
            .expect("visible Retry button")
            .emit_clicked();
        pump_until(|| widget_contains_text(native.upcast_ref(), "600"));
        assert_eq!(
            controller.window.visible_page(),
            super::ApplicationPage::Settings
        );
        ApplicationController::navigate(&controller, super::ApplicationPage::Tvs);
        ApplicationController::navigate(&controller, super::ApplicationPage::Settings);
        pump_until(|| widget_contains_text(native.upcast_ref(), "120"));
        controller.shutdown();
        controller.window.close();
    }

    fn run_diagnostics_scenario() {
        use lg_buddy::diagnostics::{DiagnosticSection, DiagnosticsReport};
        use lg_buddy::diagnostics_view::{DiagnosticsBackend, DiagnosticsError, DiagnosticsIntent};
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Collector {
            calls: AtomicUsize,
            replies: Mutex<mpsc::Receiver<DiagnosticsReport>>,
        }
        impl DiagnosticsBackend for Collector {
            fn collect(&self) -> Result<DiagnosticsReport, DiagnosticsError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(self.replies.lock().unwrap().recv().unwrap())
            }
        }

        let (reply_tx, reply_rx) = mpsc::channel();
        let backend = Arc::new(Collector {
            calls: AtomicUsize::new(0),
            replies: Mutex::new(reply_rx),
        });
        let application = test_application("Diagnostics");
        let (controller, opening) = ApplicationController::with_all_backends(
            &application,
            Arc::new(PanicBackend),
            Arc::new(EmptyTvsBackend),
            Arc::new(lg_buddy::pairing::EnvironmentPairingBackend),
            Arc::new(DefaultSettingsBackend),
            Arc::new(lg_buddy::update_flow::EnvironmentUpdateInstallBackend),
            backend.clone(),
        );
        assert!(opening.diagnostics().is_none());
        assert!(!opening.navigation().tabs_visible());
        controller.present();
        pump_for(Duration::from_millis(30));
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        let native = controller.window.window();
        native.activate_action("win.diagnostics", None).unwrap();
        pump_until(|| backend.calls.load(Ordering::SeqCst) == 1);
        assert!(widget_contains_text(
            native.upcast_ref(),
            "Collecting diagnostics"
        ));
        ApplicationController::handle_diagnostics_intent(&controller, DiagnosticsIntent::Refresh);
        pump_for(Duration::from_millis(30));
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);

        let report = DiagnosticsReport::new(
            1_000,
            vec![
                DiagnosticSection::new("Application", "Fixture diagnostics"),
                DiagnosticSection::new("TV observation", "No TV configured"),
                DiagnosticSection::new("Services", "Inspection unavailable"),
            ],
        );
        reply_tx.send(report.clone()).unwrap();
        pump_until(|| widget_contains_text(native.upcast_ref(), "Fixture diagnostics"));
        ApplicationController::handle_diagnostics_intent(&controller, DiagnosticsIntent::Copy);
        let copied = glib::MainContext::default()
            .block_on(native.clipboard().read_text_future())
            .unwrap();
        assert_eq!(copied.as_deref(), Some(report.text()));

        // Supply the native chooser's completion directly so this scenario
        // exercises the real export worker without interacting with a portal.
        let request = controller
            .application
            .borrow_mut()
            .handle_diagnostics_intent(DiagnosticsIntent::Save)
            .unwrap()
            .diagnostics()
            .unwrap()
            .save_request()
            .unwrap();
        let path = std::env::temp_dir().join(format!(
            "lg-buddy-diagnostics-controller-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        ApplicationController::handle_diagnostics_intent(
            &controller,
            DiagnosticsIntent::SaveDestination {
                request,
                path: Some(path.clone()),
            },
        );
        pump_until(|| std::fs::read_to_string(&path).ok().as_deref() == Some(report.text()));
        pump_for(Duration::from_millis(30));
        std::fs::remove_file(path).unwrap();

        ApplicationController::handle_diagnostics_intent(&controller, DiagnosticsIntent::Refresh);
        pump_until(|| backend.calls.load(Ordering::SeqCst) == 2);
        ApplicationController::handle_diagnostics_intent(&controller, DiagnosticsIntent::Close);
        reply_tx.send(report).unwrap();
        pump_for(Duration::from_millis(30));
        controller.shutdown();
        native.close();
    }

    fn run_manual_update_check_scenario() {
        use lg_buddy::presentation::update_check::UpdateCheckReport;
        use lg_buddy::settings_view::{SettingsBackend, UpdateCheckError};
        use lg_buddy::updates::UpdateChannel;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Checker {
            path: std::path::PathBuf,
            started: mpsc::Sender<UpdateChannel>,
            replies: Mutex<mpsc::Receiver<Result<bool, UpdateCheckError>>>,
            calls: AtomicUsize,
        }
        impl SettingsBackend for Checker {
            fn read_settings(
                &self,
            ) -> Result<
                Vec<lg_buddy::presentation::settings::SettingsGroup>,
                lg_buddy::settings_view::SettingsReadError,
            > {
                Ok(
                    lg_buddy::presentation::settings::SettingsPresentation::from_store(
                        &lg_buddy::settings::SettingsStore::load(&self.path).unwrap(),
                    )
                    .groups()
                    .to_vec(),
                )
            }

            fn write_setting(
                &self,
                _: lg_buddy::settings_view::SettingsMutationOperation,
                _: &mut dyn FnMut(lg_buddy::settings::SettingsMutationStage),
            ) -> Result<
                lg_buddy::settings::SettingsMutationOutcome,
                lg_buddy::settings::SettingsMutationFailure,
            > {
                panic!("checking must not change preferences or services")
            }

            fn check_for_updates(&self) -> Result<UpdateCheckReport, UpdateCheckError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let store = lg_buddy::settings::SettingsStore::load(&self.path).unwrap();
                let channel = match store
                    .effective_by_name("updates.channel")
                    .unwrap()
                    .required_value()
                    .unwrap()
                    .as_enum()
                    .unwrap()
                {
                    "stable" => UpdateChannel::Stable,
                    "prerelease" => UpdateChannel::Prerelease,
                    _ => unreachable!(),
                };
                self.started.send(channel).unwrap();
                let available = self.replies.lock().unwrap().recv().unwrap()?;
                Ok(UpdateCheckReport {
                    installed_version: "1.6.0".into(),
                    channel,
                    update_available: available,
                    warning: None,
                })
            }
        }

        fn find_button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                if button.label().as_deref() == Some(label) && button.is_visible() {
                    return Some(button.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(button) = find_button(&widget, label) {
                    return Some(button);
                }
                child = widget.next_sibling();
            }
            None
        }

        let path =
            std::env::temp_dir().join(format!("lg-buddy-update-check-{}.env", std::process::id()));
        std::fs::write(
            &path,
            "updates_auto_check=disabled\nupdates_channel=stable\n",
        )
        .unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let backend = Arc::new(Checker {
            path: path.clone(),
            started: started_tx,
            replies: Mutex::new(reply_rx),
            calls: AtomicUsize::new(0),
        });
        let application = test_application("ManualUpdates");
        let (controller, opening) = ApplicationController::new(
            &application,
            Arc::new(PanicBackend),
            Arc::new(EmptyTvsBackend),
            backend.clone(),
        );
        configure_application_navigation(&controller, &opening);
        ApplicationController::navigate(&controller, super::ApplicationPage::Settings);
        controller.present();
        let native = controller.window.window();
        pump_until(|| find_button(native.upcast_ref(), "Check for updates").is_some());
        let check = find_button(native.upcast_ref(), "Check for updates").unwrap();
        check.emit_clicked();
        pump_until(|| backend.calls.load(Ordering::SeqCst) == 1);
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            UpdateChannel::Stable
        );
        assert!(!check.is_sensitive());
        ApplicationController::handle_settings_intent(
            &controller,
            lg_buddy::settings_view::SettingsIntent::CheckForUpdates,
        );
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);

        // Navigation and refresh remain responsive while the worker is blocked.
        std::fs::write(
            &path,
            "updates_auto_check=disabled\nupdates_channel=prerelease\n",
        )
        .unwrap();
        let saved = std::fs::read(&path).unwrap();
        ApplicationController::navigate(&controller, super::ApplicationPage::Tvs);
        ApplicationController::navigate(&controller, super::ApplicationPage::Settings);
        pump_until(|| widget_contains_text(native.upcast_ref(), "Prerelease"));
        reply_tx.send(Ok(true)).unwrap();
        pump_until(|| check.is_sensitive());
        assert_eq!(check.label().as_deref(), Some("Check for updates"));
        assert!(
            !widget_contains_text(native.upcast_ref(), "Update available"),
            "an old-channel result must not replace the current row"
        );
        assert_eq!(std::fs::read(&path).unwrap(), saved);

        check.emit_clicked();
        pump_until(|| backend.calls.load(Ordering::SeqCst) == 2);
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            UpdateChannel::Prerelease
        );
        reply_tx
            .send(Err(lg_buddy::updates::UpdatesError::Http {
                url: "https://api.github.com".into(),
                message: "test offline".into(),
            }
            .into()))
            .unwrap();
        pump_until(|| find_button(native.upcast_ref(), "Copy details").is_some());
        assert!(widget_contains_text(
            native.upcast_ref(),
            "Could not check for updates"
        ));
        assert!(!widget_contains_text(
            native.upcast_ref(),
            "Update available"
        ));
        find_button(native.upcast_ref(), "Check for updates")
            .unwrap()
            .emit_clicked();
        pump_until(|| backend.calls.load(Ordering::SeqCst) == 3);
        reply_tx.send(Ok(false)).unwrap();
        pump_until(|| widget_contains_text(native.upcast_ref(), "Already up to date"));
        assert_eq!(check.label().as_deref(), Some("Check for updates"));
        assert!(!widget_contains_text(
            native.upcast_ref(),
            "No newer release available"
        ));
        assert_eq!(std::fs::read(&path).unwrap(), saved);
        controller.shutdown();
        controller.window.close();
        std::fs::remove_file(path).unwrap();
    }

    fn run_update_install_scenario() {
        use adw::prelude::AdwApplicationWindowExt;
        use lg_buddy::presentation::update_check::UpdateCheckReport;
        use lg_buddy::update_flow::{
            UpdateInstallBackend, UpdateInstallFailure, UpdateInstallOperation,
            UpdateInstallOutcome, UpdateInstallTask,
        };
        use lg_buddy::update_install::{
            InstalledUpdate, PreparedUpdateInstall, UpdateInstallError, UpdateInstallStage,
        };
        use lg_buddy::updates::UpdateChannel;
        use std::sync::atomic::{AtomicUsize, Ordering};

        fn prepared() -> PreparedUpdateInstall {
            PreparedUpdateInstall::from_parts(
                lg_buddy::version::VersionInfo::current(),
                "1.7.1".parse().unwrap(),
                UpdateChannel::Stable,
                "https://github.com/Staphylococcus/LG_Buddy/releases/tag/v1.7.1",
                "v1.7.1",
                "x86_64-unknown-linux-musl",
                "a".repeat(40),
            )
        }
        struct Settings;
        impl lg_buddy::settings_view::SettingsBackend for Settings {
            fn read_settings(
                &self,
            ) -> Result<
                Vec<lg_buddy::presentation::settings::SettingsGroup>,
                lg_buddy::settings_view::SettingsReadError,
            > {
                let store = lg_buddy::settings::ConfigEnvReader::parse(
                    "/tmp/unused-gui-update.env",
                    "updates_auto_check=disabled\nupdates_channel=stable\n",
                )
                .into_store();
                Ok(
                    lg_buddy::presentation::settings::SettingsPresentation::from_store(&store)
                        .groups()
                        .to_vec(),
                )
            }
            fn check_for_updates(
                &self,
            ) -> Result<UpdateCheckReport, lg_buddy::settings_view::UpdateCheckError> {
                Ok(UpdateCheckReport {
                    installed_version: "1.6.0".into(),
                    channel: UpdateChannel::Stable,
                    update_available: true,
                    warning: None,
                })
            }
            fn write_setting(
                &self,
                _: lg_buddy::settings_view::SettingsMutationOperation,
                _: &mut dyn FnMut(lg_buddy::settings::SettingsMutationStage),
            ) -> Result<
                lg_buddy::settings::SettingsMutationOutcome,
                lg_buddy::settings::SettingsMutationFailure,
            > {
                panic!("update workflow must not change saved settings")
            }
        }
        enum WorkerReply {
            BeginInstall,
            Done(bool),
        }
        struct Updater {
            replies: Mutex<mpsc::Receiver<WorkerReply>>,
            preparations: AtomicUsize,
            installs: AtomicUsize,
            handoffs: AtomicUsize,
        }
        impl UpdateInstallBackend for Updater {
            fn run(
                &self,
                operation: &UpdateInstallOperation,
                progress: &mut dyn FnMut(UpdateInstallStage),
            ) -> Result<UpdateInstallOutcome, UpdateInstallFailure> {
                match operation.task() {
                    UpdateInstallTask::Prepare { .. } => {
                        if self.preparations.fetch_add(1, Ordering::SeqCst) == 0 {
                            Ok(UpdateInstallOutcome::UpToDate)
                        } else {
                            Ok(UpdateInstallOutcome::Prepared(prepared()))
                        }
                    }
                    UpdateInstallTask::Install { cancellation, .. } => {
                        self.installs.fetch_add(1, Ordering::SeqCst);
                        progress(UpdateInstallStage::Acquiring);
                        loop {
                            match self.replies.lock().unwrap().recv().unwrap() {
                                WorkerReply::BeginInstall => {
                                    cancellation.claim_installer_boundary()?;
                                    progress(UpdateInstallStage::Installing);
                                }
                                WorkerReply::Done(false) => {
                                    return Err(UpdateInstallError::AuthorizationDeclined.into())
                                }
                                WorkerReply::Done(true) => {
                                    progress(UpdateInstallStage::VerifyingInstalled);
                                    return Ok(UpdateInstallOutcome::Installed(
                                        InstalledUpdate::from_parts(
                                            "1.7.1".parse().unwrap(),
                                            UpdateChannel::Stable,
                                            "v1.7.1",
                                            "x86_64-unknown-linux-musl",
                                            "a".repeat(40),
                                            "/usr/bin/lg-buddy",
                                            "/usr/bin/lg-buddy-gui",
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                    UpdateInstallTask::Relaunch(_) => panic!("handoff is a main-thread effect"),
                }
            }
            fn relaunch(&self, installed: &InstalledUpdate) -> Result<(), UpdateInstallFailure> {
                assert_eq!(
                    installed.gui_path(),
                    std::path::Path::new("/usr/bin/lg-buddy-gui")
                );
                assert_eq!(installed.identity().version().to_string(), "1.7.1");
                self.handoffs.fetch_add(1, Ordering::SeqCst);
                Err(UpdateInstallFailure::stopped())
            }
        }
        fn button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                if button.is_visible() && button.label().as_deref() == Some(label) {
                    return Some(button.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(button) = button(&widget, label) {
                    return Some(button);
                }
                child = widget.next_sibling();
            }
            None
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        let updater = Arc::new(Updater {
            replies: Mutex::new(reply_rx),
            preparations: AtomicUsize::new(0),
            installs: AtomicUsize::new(0),
            handoffs: AtomicUsize::new(0),
        });
        let application = test_application("UpdateInstall");
        let (controller, opening) = ApplicationController::with_update_backend(
            &application,
            Arc::new(PanicBackend),
            Arc::new(EmptyTvsBackend),
            Arc::new(lg_buddy::pairing::EnvironmentPairingBackend),
            Arc::new(Settings),
            updater.clone(),
        );
        configure_application_navigation(&controller, &opening);
        ApplicationController::navigate(&controller, super::ApplicationPage::Settings);
        controller.present();
        let native = controller.window.window();
        pump_until(|| {
            button(native.upcast_ref(), "Check for updates")
                .is_some_and(|button| button.is_mapped())
        });
        button(native.upcast_ref(), "Check for updates")
            .unwrap()
            .emit_clicked();
        pump_until(|| button(native.upcast_ref(), "Install update…").is_some());
        button(native.upcast_ref(), "Install update…")
            .unwrap()
            .emit_clicked();
        pump_until(|| button(native.upcast_ref(), "Check for updates").is_some());
        pump_until(|| {
            native
                .downcast_ref::<adw::ApplicationWindow>()
                .unwrap()
                .visible_dialog()
                .is_none()
        });
        assert!(widget_contains_text(
            native.upcast_ref(),
            "Already up to date"
        ));
        assert_eq!(updater.installs.load(Ordering::SeqCst), 0);
        button(native.upcast_ref(), "Check for updates")
            .unwrap()
            .emit_clicked();
        pump_until(|| button(native.upcast_ref(), "Install update…").is_some());
        button(native.upcast_ref(), "Install update…")
            .unwrap()
            .emit_clicked();
        pump_until(|| button(native.upcast_ref(), "Install and restart").is_some());
        assert!(widget_contains_text(
            native.upcast_ref(),
            "Install LG Buddy 1.7.1?"
        ));
        assert_eq!(updater.installs.load(Ordering::SeqCst), 0);
        button(native.upcast_ref(), "Cancel")
            .unwrap()
            .emit_clicked();
        assert_eq!(updater.installs.load(Ordering::SeqCst), 0);
        for success in [false, true] {
            pump_until(|| {
                native
                    .downcast_ref::<adw::ApplicationWindow>()
                    .unwrap()
                    .visible_dialog()
                    .is_none()
            });
            button(native.upcast_ref(), "Install update…")
                .unwrap()
                .emit_clicked();
            pump_until(|| button(native.upcast_ref(), "Install and restart").is_some());
            button(native.upcast_ref(), "Install and restart")
                .unwrap()
                .emit_clicked();
            pump_until(|| updater.installs.load(Ordering::SeqCst) == if success { 2 } else { 1 });
            ApplicationController::handle_settings_intent(
                &controller,
                lg_buddy::settings_view::SettingsIntent::ConfirmUpdateInstall,
            );
            ApplicationController::navigate(&controller, super::ApplicationPage::Tvs);
            ApplicationController::navigate(&controller, super::ApplicationPage::Settings);
            reply_tx.send(WorkerReply::BeginInstall).unwrap();
            pump_until(|| widget_contains_text(native.upcast_ref(), "Installing update…"));
            ApplicationController::handle_intent(&controller, super::OverviewIntent::Cancel);
            assert!(
                !controller.closed.get(),
                "claimed installation keeps progress and errors visible"
            );
            assert!(button(native.upcast_ref(), "Cancel").is_none());
            reply_tx.send(WorkerReply::Done(success)).unwrap();
            pump_until(|| {
                native
                    .downcast_ref::<adw::ApplicationWindow>()
                    .unwrap()
                    .visible_dialog()
                    .is_none()
            });
            pump_until(|| button(native.upcast_ref(), "Copy details").is_some());
            assert!(button(native.upcast_ref(), "Install update…").is_some());
        }
        assert_eq!(updater.installs.load(Ordering::SeqCst), 2);
        assert_eq!(updater.handoffs.load(Ordering::SeqCst), 1);
        button(native.upcast_ref(), "Install update…")
            .unwrap()
            .emit_clicked();
        assert_eq!(updater.handoffs.load(Ordering::SeqCst), 2);
        assert_eq!(
            updater.installs.load(Ordering::SeqCst),
            2,
            "restart retry cannot reinstall"
        );
        assert_eq!(application.windows().len(), 1);
        controller.shutdown();
        controller.window.close();
    }

    fn run_settings_write_scenario() {
        use adw::prelude::{ComboRowExt, PreferencesRowExt};
        use lg_buddy::settings::{
            execute_settings_mutation, SettingsApplier, SettingsMutation, SettingsMutationFailure,
            SettingsMutationOutcome, SettingsMutationStage, SettingsStore,
        };
        use lg_buddy::settings_view::{
            BehaviorSetting, SettingsMutationOperation, SettingsMutationRequest,
        };
        struct SettingsWriter {
            path: std::path::PathBuf,
            started: mpsc::Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
            panic_after_save: bool,
        }
        impl lg_buddy::settings_view::SettingsBackend for SettingsWriter {
            fn check_for_updates(
                &self,
            ) -> Result<
                lg_buddy::presentation::update_check::UpdateCheckReport,
                lg_buddy::settings_view::UpdateCheckError,
            > {
                panic!("unexpected update check")
            }

            fn read_settings(
                &self,
            ) -> Result<
                Vec<lg_buddy::presentation::settings::SettingsGroup>,
                lg_buddy::settings_view::SettingsReadError,
            > {
                Ok(
                    lg_buddy::presentation::settings::SettingsPresentation::from_store(
                        &SettingsStore::load(&self.path).unwrap(),
                    )
                    .groups()
                    .to_vec(),
                )
            }
            fn write_setting(
                &self,
                operation: SettingsMutationOperation,
                progress: &mut dyn FnMut(SettingsMutationStage),
            ) -> Result<SettingsMutationOutcome, SettingsMutationFailure> {
                progress(SettingsMutationStage::Validating);
                self.started.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
                let SettingsMutationRequest::Set(value) = operation.request() else {
                    unreachable!()
                };
                let mutation = SettingsMutation::set(
                    &SettingsStore::load(&self.path).unwrap(),
                    operation.key_name(),
                    value,
                )
                .unwrap();
                // updates.channel has no systemd action, so this fake never touches host services.
                let result = execute_settings_mutation(
                    &self.path,
                    mutation,
                    &SettingsApplier::from_env(),
                    progress,
                );
                assert!(
                    !self.panic_after_save,
                    "test worker stopped after publication"
                );
                result
            }
        }
        fn update_channel(widget: &gtk::Widget) -> Option<adw::ComboRow> {
            if let Some(row) = widget.downcast_ref::<adw::ComboRow>() {
                if row.title() == "Update channel" {
                    return Some(row.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(row) = update_channel(&current) {
                    return Some(row);
                }
                child = current.next_sibling();
            }
            None
        }
        for (suffix, panic_after_save, close_pending, queued) in [
            ("SettingsWrite", false, false, false),
            ("SettingsStopped", true, false, false),
            ("SettingsClose", false, true, false),
            ("SettingsQueuedClose", false, true, true),
        ] {
            let path =
                std::env::temp_dir().join(format!("lg-buddy-{suffix}-{}.env", std::process::id()));
            std::fs::write(&path, "updates_channel=stable\n").unwrap();
            let application = test_application(suffix);
            let (started_tx, started_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let (controller, opening) = ApplicationController::new(
                &application,
                Arc::new(PanicBackend),
                Arc::new(EmptyTvsBackend),
                Arc::new(SettingsWriter {
                    path: path.clone(),
                    started: started_tx,
                    release: Mutex::new(release_rx),
                    panic_after_save,
                }),
            );
            configure_application_navigation(&controller, &opening);
            ApplicationController::navigate(&controller, super::ApplicationPage::Settings);
            controller.present();
            let native = controller.window.window();
            pump_until(|| widget_contains_text(native.upcast_ref(), "Stable"));
            ApplicationController::handle_settings_intent(
                &controller,
                lg_buddy::settings_view::SettingsIntent::Commit {
                    setting: BehaviorSetting::UpdatesChannel,
                    value: "prerelease".into(),
                },
            );
            pump_until(|| started_rx.try_recv().is_ok());
            assert!(std::fs::read_to_string(&path).unwrap().contains("stable"));
            assert!(update_channel(native.upcast_ref()).unwrap().is_sensitive());
            if queued {
                ApplicationController::handle_settings_intent(
                    &controller,
                    lg_buddy::settings_view::SettingsIntent::Commit {
                        setting: BehaviorSetting::UpdatesChannel,
                        value: "stable".into(),
                    },
                );
            }
            pump_for(Duration::from_millis(20));
            if close_pending {
                ApplicationController::handle_intent(&controller, OverviewIntent::Cancel);
                assert!(controller.closed.get());
            }
            release_tx.send(()).unwrap();
            pump_until(|| {
                std::fs::read_to_string(&path)
                    .unwrap()
                    .contains("prerelease")
            });
            if queued {
                pump_until(|| started_rx.try_recv().is_ok());
                release_tx.send(()).unwrap();
                pump_until(|| std::fs::read_to_string(&path).unwrap().contains("stable"));
            }
            if close_pending {
                pump_for(Duration::from_millis(30));
                assert!(
                    controller.closed.get(),
                    "late completion must not reopen Settings"
                );
            } else {
                pump_until(|| widget_contains_text(native.upcast_ref(), "Prerelease"));
                if panic_after_save {
                    pump_until(|| {
                        widget_contains_text(native.upcast_ref(), "The setting was saved, but runtime apply could not be confirmed. Retry apply.")
                    });
                } else {
                    pump_until(|| {
                        update_channel(native.upcast_ref())
                            .is_some_and(|row| row.selected() == 1 && row.is_sensitive())
                    });
                }
                controller.shutdown();
            }
            controller.window.close();
            std::fs::remove_file(path).unwrap();
        }
    }

    fn run_pairing_scenario() {
        use lg_buddy::pairing::{
            PairingBackend, PairingError, PairingFailure, PairingIntent, PairingOperation,
            PairingOutcome, PairingStage,
        };
        use lg_buddy::presentation::settings::{SettingsGroup, SettingsPresentation};
        use lg_buddy::settings::{
            execute_settings_mutation, ServiceController, SettingsApplier, SettingsError,
            SettingsMutation, SettingsMutationFailure, SettingsMutationStage, SettingsStore,
            UserServiceState, UserUnitEnableOutcome,
        };
        use lg_buddy::settings_view::{
            BehaviorSetting, SettingsBackend, SettingsMutationOperation, SettingsMutationRequest,
            SettingsReadError,
        };
        use lg_buddy::tvs::{TvCredentialState, TvId, TvProfile, TvsIntent};
        struct PairingMock {
            release: Mutex<mpsc::Receiver<()>>,
            reject: bool,
            panic: bool,
            requested_behaviors: Vec<BehaviorSetting>,
        }
        impl PairingBackend for PairingMock {
            fn pair(
                &self,
                operation: &PairingOperation,
                progress: &mut dyn FnMut(PairingStage),
            ) -> Result<PairingOutcome, PairingError> {
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
                let profile = TvProfile::new(
                    TvId::primary(),
                    "Primary TV",
                    request.address(),
                    request.mac(),
                    request.input(),
                    TvPlatform::LgWebOs,
                    TvCredentialState::Stored,
                );
                Ok(PairingOutcome::new(
                    profile,
                    self.requested_behaviors.clone(),
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

        struct ActiveScreenService;

        impl ServiceController for ActiveScreenService {
            fn user_service_state(&self, service: &str) -> Result<UserServiceState, SettingsError> {
                assert_eq!(service, "LG_Buddy_screen.service");
                Ok(UserServiceState::ActiveOrEnabled)
            }

            fn restart_user_service(&self, service: &str) -> Result<(), SettingsError> {
                assert_eq!(service, "LG_Buddy_screen.service");
                Ok(())
            }

            fn enable_start_user_unit(
                &self,
                service: &str,
            ) -> Result<UserUnitEnableOutcome, SettingsError> {
                assert_eq!(service, "LG_Buddy_screen.service");
                Ok(UserUnitEnableOutcome::EnabledStarted)
            }

            fn disable_stop_user_unit(&self, service: &str) -> Result<(), SettingsError> {
                assert_eq!(service, "LG_Buddy_screen.service");
                Ok(())
            }
        }

        struct ActivationSettingsBackend {
            path: std::path::PathBuf,
            requests: Arc<Mutex<Vec<BehaviorSetting>>>,
        }

        impl SettingsBackend for ActivationSettingsBackend {
            fn read_settings(&self) -> Result<Vec<SettingsGroup>, SettingsReadError> {
                let store = SettingsStore::load(&self.path)
                    .map_err(|error| SettingsReadError::unreadable(error.to_string()))?;
                Ok(SettingsPresentation::from_store(&store).groups().to_vec())
            }

            fn check_for_updates(
                &self,
            ) -> Result<
                lg_buddy::presentation::update_check::UpdateCheckReport,
                lg_buddy::settings_view::UpdateCheckError,
            > {
                panic!("unexpected update check during pairing")
            }

            fn write_setting(
                &self,
                operation: SettingsMutationOperation,
                progress: &mut dyn FnMut(SettingsMutationStage),
            ) -> Result<lg_buddy::settings::SettingsMutationOutcome, SettingsMutationFailure>
            {
                let setting = operation.setting();
                self.requests.lock().unwrap().push(setting);
                if setting == BehaviorSetting::SystemSleepWakePolicy {
                    return Err(SettingsMutationFailure::Activation(
                        SettingsError::ActivationCancelled,
                    ));
                }
                let SettingsMutationRequest::Set(value) = operation.request() else {
                    panic!("pairing defaults must submit enabled values")
                };
                let store = SettingsStore::load(&self.path)
                    .map_err(SettingsMutationFailure::Persistence)?;
                let mutation = SettingsMutation::set(&store, operation.key_name(), value)
                    .map_err(SettingsMutationFailure::Validation)?;
                execute_settings_mutation(
                    &self.path,
                    mutation,
                    &SettingsApplier::new(ActiveScreenService),
                    progress,
                )
            }
        }

        fn switch_state(widget: &gtk::Widget, title: &str) -> Option<bool> {
            if let Some(row) = widget.downcast_ref::<adw::SwitchRow>() {
                if row.title() == title {
                    return Some(row.is_active());
                }
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(state) = switch_state(&current, title) {
                    return Some(state);
                }
                child = current.next_sibling();
            }
            None
        }
        for (cancel, reject, panic, requested_behaviors, name) in [
            (
                false,
                false,
                false,
                Vec::<BehaviorSetting>::new(),
                "PairSuccess",
            ),
            (
                true,
                false,
                false,
                Vec::<BehaviorSetting>::new(),
                "PairCancel",
            ),
            (
                false,
                true,
                false,
                Vec::<BehaviorSetting>::new(),
                "PairRejected",
            ),
            (
                false,
                false,
                true,
                Vec::<BehaviorSetting>::new(),
                "PairWorkerStopped",
            ),
            (
                false,
                false,
                false,
                vec![
                    BehaviorSetting::ScreenIdleBlank,
                    BehaviorSetting::SystemSleepWakePolicy,
                ],
                "PairActivationCancelled",
            ),
        ] {
            let application = test_application(name);
            let (backend, controls) = BlockingBackend::new();
            let (release, receiver) = mpsc::channel();
            let settings_path = std::env::temp_dir().join(format!(
                "lg-buddy-{name}-settings-{}.env",
                std::process::id()
            ));
            std::fs::write(
                &settings_path,
                "screen_idle_blank=disabled\nsystem_sleep_wake_policy=disabled\n",
            )
            .unwrap();
            let settings_requests = Arc::new(Mutex::new(Vec::new()));
            let pairing_backend = Arc::new(PairingMock {
                release: Mutex::new(receiver),
                reject,
                panic,
                requested_behaviors: requested_behaviors.clone(),
            });
            let settings_backend: Arc<dyn SettingsBackend> = Arc::new(ActivationSettingsBackend {
                path: settings_path.clone(),
                requests: Arc::clone(&settings_requests),
            });
            let (controller, opening) = ApplicationController::with_backends(
                &application,
                Arc::new(backend),
                Arc::new(TvsMock),
                pairing_backend,
                settings_backend,
            );
            ApplicationController::render_tvs_transition(&controller, opening.tvs().unwrap());
            controller.present();
            ApplicationController::navigate(
                &controller,
                lg_buddy::navigation::ApplicationPage::Tvs,
            );
            pump_until(|| {
                widget_contains_text(controller.window.window().upcast_ref(), "No TV configured")
            });
            assert!(!controller.window.navigation_visible());
            assert!(controller.window.main_menu_visible());
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
                    controller.window.visible_page(),
                    lg_buddy::navigation::ApplicationPage::Overview
                );
                assert!(controller.window.navigation_visible());
                if !requested_behaviors.is_empty() {
                    pump_until(|| {
                        let requests = settings_requests.lock().unwrap();
                        requests.len() == 2
                            && switch_state(
                                controller.window.window().upcast_ref(),
                                "Idle blanking",
                            ) == Some(true)
                            && switch_state(
                                controller.window.window().upcast_ref(),
                                "TV sleep & wake",
                            ) == Some(false)
                    });
                    assert_eq!(
                        *settings_requests.lock().unwrap(),
                        vec![
                            BehaviorSetting::ScreenIdleBlank,
                            BehaviorSetting::SystemSleepWakePolicy,
                        ]
                    );
                    assert!(!widget_contains_text(
                        controller.window.window().upcast_ref(),
                        "Retry setup",
                    ));
                    assert!(controller.window.main_menu_visible());
                    assert!(controller.window.navigation_visible());
                }
            }
            assert!(
                !widget_contains_text(
                    controller.window.window().upcast_ref(),
                    "TV paired successfully",
                ),
                "pairing completion does not present a success toast",
            );
            ApplicationController::handle_intent(&controller, OverviewIntent::Cancel);
            assert!(controller.closed.get());
            assert!(
                !controller.window.window().is_visible(),
                "application quit must close the window even when pairing has a dialog open",
            );
            let _ = std::fs::remove_file(settings_path);
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
            Arc::new(DefaultSettingsBackend),
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
