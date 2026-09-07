use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use lg_buddy::navigation::ApplicationPage;
use lg_buddy::overview::OverviewIntent;
use lg_buddy::presentation::overview::OverviewPresentation;
use lg_buddy::presentation::tvs::TvsPresentation;
use lg_buddy::tvs::TvsIntent;

pub(crate) struct ApplicationWindow {
    window: adw::ApplicationWindow,
    overview: crate::overview::OverviewView,
    tvs: crate::tvs::TvsView,
    pairing: crate::pairing::PairingView,
    toasts: adw::ToastOverlay,
    stack: adw::ViewStack,
    suppress_navigation: Rc<Cell<bool>>,
    allow_close: Rc<Cell<bool>>,
    close_requested: Rc<Cell<bool>>,
}

impl ApplicationWindow {
    pub(crate) fn new(
        application: &adw::Application,
        on_overview: crate::overview::IntentHandler,
        on_tvs: Rc<dyn Fn(TvsIntent)>,
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
        let overview = crate::overview::OverviewView::new(&window, Rc::clone(&on_overview));
        let tvs = crate::tvs::TvsView::new(Rc::clone(&on_tvs));
        let pairing = crate::pairing::PairingView::new(on_tvs);
        let stack = adw::ViewStack::new();
        stack.set_hhomogeneous(false);
        stack.set_vhomogeneous(false);
        for page in ApplicationPage::ALL {
            let (widget, icon): (&gtk::Widget, _) = match page {
                ApplicationPage::Overview => (overview.widget().upcast_ref(), "view-grid-symbolic"),
                ApplicationPage::Tvs => (tvs.widget().upcast_ref(), "video-display-symbolic"),
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
        let switcher_bar = adw::ViewSwitcherBar::builder().stack(&stack).build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));
        let toolbar = adw::ToolbarView::builder().content(&toasts).build();
        toolbar.add_top_bar(&header);
        toolbar.add_bottom_bar(&switcher_bar);
        window.set_content(Some(&toolbar));
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            400.0,
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
            pairing,
            toasts,
            stack,
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
        if page != ApplicationPage::Overview {
            self.overview.leave();
        }
        self.suppress_navigation.set(true);
        self.stack.set_visible_child_name(page_name(page));
        self.suppress_navigation.set(false);
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

    #[cfg(test)]
    pub(crate) fn choose_page(&self, page: ApplicationPage) {
        self.stack.set_visible_child_name(page_name(page));
    }
}

fn page_name(page: ApplicationPage) -> &'static str {
    match page {
        ApplicationPage::Overview => "overview",
        ApplicationPage::Tvs => "tvs",
    }
}
