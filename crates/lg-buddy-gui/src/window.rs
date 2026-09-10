use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use lg_buddy::navigation::ApplicationPage;
use lg_buddy::overview::OverviewIntent;
use lg_buddy::presentation::overview::OverviewPresentation;
use lg_buddy::presentation::settings::SettingsPresentation;
use lg_buddy::presentation::tvs::TvsPresentation;
use lg_buddy::settings_view::SettingsIntent;
use lg_buddy::tvs::TvsIntent;

pub(crate) struct ApplicationWindow {
    window: adw::ApplicationWindow,
    overview: crate::overview::OverviewView,
    tvs: crate::tvs::TvsView,
    settings: crate::settings::SettingsView,
    pairing: crate::pairing::PairingView,
    toasts: adw::ToastOverlay,
    stack: adw::ViewStack,
    switcher: adw::ViewSwitcher,
    switcher_bar: adw::ViewSwitcherBar,
    #[cfg(test)]
    menu_button: gtk::MenuButton,
    suppress_navigation: Rc<Cell<bool>>,
    allow_close: Rc<Cell<bool>>,
    close_requested: Rc<Cell<bool>>,
}

impl ApplicationWindow {
    pub(crate) fn new(
        application: &adw::Application,
        on_overview: crate::overview::IntentHandler,
        on_tvs: Rc<dyn Fn(TvsIntent)>,
        on_settings: Rc<dyn Fn(SettingsIntent)>,
        on_navigation: Rc<dyn Fn(ApplicationPage)>,
    ) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title(crate::APPLICATION_NAME)
            .icon_name(crate::APPLICATION_ID)
            .default_width(600)
            .default_height(420)
            .width_request(280)
            .height_request(240)
            .build();
        crate::register_resources();
        gtk::IconTheme::for_display(&gtk::prelude::WidgetExt::display(&window))
            .add_resource_path("/io/github/staphylococcus/LGBuddy/icons");
        let about = gtk::gio::SimpleAction::new("about", None);
        about.connect_activate({
            let window = window.downgrade();
            move |_, _| {
                if let Some(window) = window.upgrade() {
                    show_about(&window);
                }
            }
        });
        window.add_action(&about);
        let overview = crate::overview::OverviewView::new(&window, Rc::clone(&on_overview));
        let tvs = crate::tvs::TvsView::new(Rc::clone(&on_tvs));
        let pairing = crate::pairing::PairingView::new(on_tvs);
        let settings = crate::settings::SettingsView::new(on_settings);
        let stack = adw::ViewStack::new();
        stack.set_hhomogeneous(false);
        stack.set_vhomogeneous(false);
        for page in ApplicationPage::ALL {
            let (widget, icon): (&gtk::Widget, _) = match page {
                ApplicationPage::Overview => (overview.widget().upcast_ref(), "view-grid-symbolic"),
                ApplicationPage::Tvs => (tvs.widget().upcast_ref(), "video-display-symbolic"),
                ApplicationPage::Settings => (
                    settings.widget().upcast_ref(),
                    "preferences-system-symbolic",
                ),
            };
            stack.add_titled_with_icon(widget, Some(page_name(page)), page.title(), icon);
        }
        stack.set_visible_child_name(page_name(ApplicationPage::Overview));
        let suppress_navigation = Rc::new(Cell::new(false));
        stack.connect_visible_child_name_notify({
            let suppress = Rc::clone(&suppress_navigation);
            move |stack| {
                if !suppress.get() {
                    match stack.visible_child_name().as_deref() {
                        Some("overview") => on_navigation(ApplicationPage::Overview),
                        Some("tvs") => on_navigation(ApplicationPage::Tvs),
                        Some("settings") => on_navigation(ApplicationPage::Settings),
                        _ => {}
                    }
                }
            }
        });
        let switcher = adw::ViewSwitcher::builder()
            .stack(&stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build();
        let header = adw::HeaderBar::builder().title_widget(&switcher).build();
        let menu = gtk::gio::Menu::new();
        menu.append(Some("About LG Buddy"), Some("win.about"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .primary(true)
            .tooltip_text("Main Menu")
            .menu_model(&menu)
            .build();
        menu_button.update_property(&[gtk::accessible::Property::Label("Main Menu")]);
        header.pack_end(&menu_button);
        let switcher_bar = adw::ViewSwitcherBar::builder().stack(&stack).build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&toasts);
        content.set_vexpand(true);
        toasts.set_vexpand(true);
        let toolbar = adw::ToolbarView::builder().content(&content).build();
        toolbar.add_top_bar(&header);
        toolbar.add_bottom_bar(&switcher_bar);
        window.set_content(Some(&toolbar));
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            540.0,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(
            &header,
            "title-widget",
            Some(&None::<gtk::Widget>.to_value()),
        );
        narrow.add_setter(&switcher_bar, "reveal", Some(&true.to_value()));
        window.add_breakpoint(narrow);

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
                on_overview(OverviewIntent::Cancel);
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
            overview,
            tvs,
            settings,
            pairing,
            toasts,
            stack,
            switcher,
            switcher_bar,
            #[cfg(test)]
            menu_button,
            suppress_navigation,
            allow_close,
            close_requested,
        }
    }

    pub(crate) fn render(&self, presentation: &OverviewPresentation) {
        self.overview.render(presentation);
    }

    pub(crate) fn render_tvs(&self, presentation: &TvsPresentation) {
        self.tvs.render(&self.window, presentation);
        self.pairing.render(&self.window, presentation.pairing());
    }

    pub(crate) fn render_settings(&self, presentation: &SettingsPresentation) {
        self.settings.render(presentation);
    }

    pub(crate) fn show_update_notice(&self, notice: &lg_buddy::settings_view::UpdateNotice) {
        self.settings.show_update_notice(notice, &self.toasts);
    }

    pub(crate) fn dismiss_dialog(&self) -> bool {
        if let Some(dialog) = self.window.visible_dialog() {
            dialog.close();
            true
        } else {
            false
        }
    }

    pub(crate) fn show_toast(&self, message: &str) {
        if !self.pairing.show_toast(message) {
            self.toasts.add_toast(adw::Toast::new(message));
        }
    }

    pub(crate) fn navigate(&self, page: ApplicationPage) {
        if self.stack.visible_child_name().as_deref() == Some(page_name(page)) {
            return;
        }
        if page != ApplicationPage::Overview {
            self.overview.leave();
        }
        self.suppress_navigation.set(true);
        self.stack.set_visible_child_name(page_name(page));
        self.suppress_navigation.set(false);
    }

    /// Render application-owned navigation availability. The app menu remains
    /// in the header while both desktop and narrow-window tab controls hide.
    pub(crate) fn set_navigation_visible(&self, visible: bool) {
        self.switcher.set_visible(visible);
        self.switcher_bar.set_visible(visible);
    }

    pub(crate) fn present(&self) {
        self.window.present();
    }

    pub(crate) fn focus_brightness(&self) {
        if self.window.visible_dialog().is_none() {
            self.overview.focus_brightness();
        }
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

    #[cfg(test)]
    pub(crate) fn visible_page(&self) -> ApplicationPage {
        match self.stack.visible_child_name().as_deref() {
            Some("overview") => ApplicationPage::Overview,
            Some("tvs") => ApplicationPage::Tvs,
            Some("settings") => ApplicationPage::Settings,
            _ => panic!("application window has no visible page"),
        }
    }

    #[cfg(test)]
    pub(crate) fn navigation_visible(&self) -> bool {
        self.switcher.is_visible() && self.switcher_bar.is_visible()
    }

    #[cfg(test)]
    pub(crate) fn main_menu_visible(&self) -> bool {
        self.menu_button.is_visible() && self.menu_button.is_sensitive()
    }
}

fn show_about(window: &adw::ApplicationWindow) {
    // ponytail: let Adwaita own the About layout, subpages, links, and dismissal.
    let dialog = adw::AboutDialog::builder()
        .application_name(crate::APPLICATION_NAME)
        .application_icon(crate::APPLICATION_ID)
        .developer_name("LG Buddy Contributors")
        .version(lg_buddy::version::VersionInfo::current().version())
        .website("https://github.com/Staphylococcus/LG_Buddy")
        .issue_url("https://github.com/Staphylococcus/LG_Buddy/issues/new/choose")
        .developers([
            "Vas Zayarskiy https://github.com/Staphylococcus",
            "Faceless3882 https://github.com/Faceless3882",
            "Contributors https://github.com/Staphylococcus/LG_Buddy/graphs/contributors",
        ])
        .license_type(gtk::License::Gpl30Only)
        .debug_info(lg_buddy::version::version_text())
        .debug_info_filename("lg-buddy-version.txt")
        .build();
    dialog.add_acknowledgement_section(
        Some("With Thanks"),
        &[
            "chros73 — bscpylgtv https://github.com/chros73/bscpylgtv",
            "JPersson77 — LGTV Companion https://github.com/JPersson77/LGTVCompanion",
        ],
    );
    dialog.add_legal_section(
        "GNOME edit-delete icon",
        None,
        gtk::License::Custom,
        Some("By Jakub Steiner, dedicated to the public domain under <a href=\"https://creativecommons.org/publicdomain/zero/1.0/\">CC0 1.0 Universal</a>."),
    );
    dialog.present(Some(window));
}

fn page_name(page: ApplicationPage) -> &'static str {
    match page {
        ApplicationPage::Overview => "overview",
        ApplicationPage::Tvs => "tvs",
        ApplicationPage::Settings => "settings",
    }
}
