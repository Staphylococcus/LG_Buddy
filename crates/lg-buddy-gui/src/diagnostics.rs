use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use lg_buddy::diagnostics_view::{DiagnosticsIntent, DiagnosticsPresentation};

/// Native renderer for the on-demand diagnostics dialog.
///
/// The report and all status decisions belong to the application. This view
/// only presents those values and forwards the semantic actions supplied by
/// the application controller.
pub(crate) struct DiagnosticsView {
    dialog: adw::Dialog,
    report: gtk::TextView,
    status: gtk::Label,
    #[cfg(test)]
    close: gtk::Button,
    refresh: gtk::Button,
    copy: gtk::Button,
    save: gtk::Button,
    #[cfg(test)]
    actions: gtk::FlowBox,
    presented: Rc<Cell<bool>>,
    suppress_close: Rc<Cell<bool>>,
}

impl DiagnosticsView {
    pub(crate) fn new(
        on_intent: Rc<dyn Fn(DiagnosticsIntent)>,
        return_focus: &gtk::Widget,
    ) -> Self {
        let presented = Rc::new(Cell::new(false));
        let suppress_close = Rc::new(Cell::new(false));

        let report = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .accepts_tab(false)
            .wrap_mode(gtk::WrapMode::WordChar)
            .monospace(true)
            .hexpand(true)
            .vexpand(true)
            .top_margin(12)
            .bottom_margin(12)
            .left_margin(12)
            .right_margin(12)
            .build();
        report.set_focusable(true);
        report.update_property(&[gtk::accessible::Property::Label("Diagnostic report")]);

        let report_heading = gtk::Label::builder()
            .label("Diagnostic report")
            .xalign(0.0)
            .build();
        report_heading.add_css_class("title-3");

        let status = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .hexpand(true)
            .visible(false)
            .build();
        status.set_accessible_role(gtk::AccessibleRole::Status);

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .hexpand(true)
            .propagate_natural_height(false)
            .child(&report)
            .build();
        scroller.update_property(&[gtk::accessible::Property::Label("Diagnostic report")]);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(20)
            .margin_end(20)
            .margin_top(20)
            .margin_bottom(20)
            .hexpand(true)
            .vexpand(true)
            .build();
        content.append(&status);
        content.append(&report_heading);
        content.append(&scroller);

        let close = gtk::Button::with_label("Close");
        close.connect_clicked({
            let on_intent = Rc::clone(&on_intent);
            move |_| on_intent(DiagnosticsIntent::Close)
        });
        let refresh = gtk::Button::with_label("Refresh");
        refresh.connect_clicked({
            let on_intent = Rc::clone(&on_intent);
            move |_| on_intent(DiagnosticsIntent::Refresh)
        });
        let copy = gtk::Button::with_label("Copy");
        copy.connect_clicked({
            let on_intent = Rc::clone(&on_intent);
            move |_| on_intent(DiagnosticsIntent::Copy)
        });
        let save = gtk::Button::with_label("Save…");
        save.connect_clicked({
            let on_intent = Rc::clone(&on_intent);
            move |_| on_intent(DiagnosticsIntent::Save)
        });
        for button in [&close, &refresh, &copy, &save] {
            button.set_width_request(80);
        }

        // FlowBox wraps the actions on narrow windows while retaining native
        // button focus and ordering.
        let actions = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .min_children_per_line(1)
            .max_children_per_line(4)
            .column_spacing(6)
            .row_spacing(6)
            .halign(gtk::Align::Fill)
            .hexpand(true)
            .build();
        actions.insert(&close, -1);
        actions.insert(&refresh, -1);
        actions.insert(&copy, -1);
        actions.insert(&save, -1);

        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        let toolbar = adw::ToolbarView::builder().content(&content).build();
        toolbar.add_top_bar(&header);
        let actions_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(20)
            .margin_end(20)
            .margin_top(12)
            .margin_bottom(20)
            .build();
        actions_bar.append(&actions);
        toolbar.add_bottom_bar(&actions_bar);

        let dialog = adw::Dialog::builder()
            .title("Diagnostics")
            .content_width(720)
            .content_height(560)
            .child(&toolbar)
            .build();
        dialog.connect_closed({
            let on_intent = Rc::clone(&on_intent);
            let presented = Rc::clone(&presented);
            let suppress_close = Rc::clone(&suppress_close);
            let return_focus = return_focus.downgrade();
            move |_| {
                presented.set(false);
                if !suppress_close.replace(false) {
                    on_intent(DiagnosticsIntent::Close);
                }
                // The menu item that opened the dialog no longer exists, and
                // a native file chooser can also clear the parent's focus.
                if let Some(return_focus) = return_focus.upgrade() {
                    return_focus.grab_focus();
                }
            }
        });

        Self {
            dialog,
            report,
            status,
            #[cfg(test)]
            close,
            refresh,
            copy,
            save,
            #[cfg(test)]
            actions,
            presented,
            suppress_close,
        }
    }

    pub(crate) fn render(
        &self,
        parent: &adw::ApplicationWindow,
        presentation: &DiagnosticsPresentation,
    ) {
        let report_text = presentation
            .report_text()
            .unwrap_or("No diagnostic report has been collected yet.");
        let buffer = self.report.buffer();
        let start = buffer.start_iter();
        let end = buffer.end_iter();
        if buffer.text(&start, &end, false).as_str() != report_text {
            buffer.set_text(report_text);
        }

        let status = if let Some(error) = presentation.error() {
            Some(error.to_owned())
        } else if presentation.collecting() {
            Some("Collecting diagnostics…".to_owned())
        } else if presentation.saving() {
            Some("Saving diagnostics…".to_owned())
        } else {
            presentation
                .collected_at()
                .map(|collected_at| format!("Collected {collected_at}"))
        };
        self.status.set_text(status.as_deref().unwrap_or(""));
        self.status.set_visible(status.is_some());
        self.status
            .set_accessible_role(if presentation.error().is_some() {
                gtk::AccessibleRole::Alert
            } else {
                gtk::AccessibleRole::Status
            });

        self.refresh.set_sensitive(presentation.can_refresh());
        self.copy.set_sensitive(presentation.can_export());
        self.save.set_sensitive(presentation.can_export());

        if presentation.visible() {
            if !self.presented.replace(true) {
                self.dialog.present(Some(parent));
            }
        } else if self.presented.replace(false) {
            self.suppress_close.set(true);
            self.dialog.force_close();
        }
    }

    #[cfg(test)]
    fn report(&self) -> &gtk::TextView {
        &self.report
    }

    #[cfg(test)]
    fn refresh(&self) -> &gtk::Button {
        &self.refresh
    }

    #[cfg(test)]
    fn close(&self) -> &gtk::Button {
        &self.close
    }

    #[cfg(test)]
    fn copy(&self) -> &gtk::Button {
        &self.copy
    }

    #[cfg(test)]
    fn save(&self) -> &gtk::Button {
        &self.save
    }
}

#[cfg(test)]
pub(crate) fn run_renderer_scenarios(application: &adw::Application) {
    use crate::controller_test_support::pump_until;
    use lg_buddy::diagnostics::{DiagnosticSection, DiagnosticsReport};
    use lg_buddy::diagnostics_view::{DiagnosticsApplication, DiagnosticsError};
    use std::cell::RefCell;
    use std::path::PathBuf;

    fn action_row_positions(actions: &gtk::FlowBox) -> Vec<i32> {
        let mut positions = Vec::new();
        let mut child = actions.first_child();
        while let Some(child_widget) = child {
            positions.push(child_widget.allocation().y());
            child = child_widget.next_sibling();
        }
        positions
    }

    let intents = Rc::new(RefCell::new(Vec::new()));
    let origin = gtk::Button::with_label("Open diagnostics");
    let view = DiagnosticsView::new(
        Rc::new({
            let intents = Rc::clone(&intents);
            move |intent| intents.borrow_mut().push(intent)
        }),
        origin.upcast_ref(),
    );
    let wide_window = adw::ApplicationWindow::builder()
        .application(application)
        .default_width(800)
        .default_height(600)
        .build();
    wide_window.set_content(Some(&origin));
    wide_window.present();

    assert!(view.report().is_focusable());
    assert_eq!(view.report().wrap_mode(), gtk::WrapMode::WordChar);
    assert!(!view.report().is_editable());
    assert_eq!(view.dialog.title(), "Diagnostics");
    assert_eq!(view.close().label().as_deref(), Some("Close"));
    assert_eq!(view.refresh().label().as_deref(), Some("Refresh"));
    assert_eq!(view.copy().label().as_deref(), Some("Copy"));
    assert_eq!(view.save().label().as_deref(), Some("Save…"));

    let mut model = DiagnosticsApplication::default();
    let opening = model.handle_intent(DiagnosticsIntent::Open).unwrap();
    view.render(&wide_window, opening.presentation());
    pump_until(|| {
        let positions = action_row_positions(&view.actions);
        view.report().width() > 400
            && positions.len() == 4
            && positions.iter().all(|position| *position == positions[0])
            && view
                .dialog
                .child()
                .map(|child| child.is_mapped())
                .unwrap_or(false)
    });
    assert!(view.presented.get());
    assert!(!view.refresh().is_sensitive());
    assert!(!view.copy().is_sensitive());
    assert!(view.report().width() > 400);
    assert_eq!(view.actions.max_children_per_line(), 4);
    assert_eq!(view.actions.min_children_per_line(), 1);
    let wide_action_rows = action_row_positions(&view.actions);
    assert_eq!(wide_action_rows.len(), 4);
    assert!(wide_action_rows
        .iter()
        .all(|position| *position == wide_action_rows[0]));
    assert_eq!(view.status.text(), "Collecting diagnostics…");

    let completed = model
        .complete_read(
            opening.read_operation().unwrap(),
            Ok(DiagnosticsReport::new(
                1_000,
                vec![DiagnosticSection::new("Application", "safe fixture")],
            )),
        )
        .unwrap();
    view.render(&wide_window, completed.presentation());
    assert!(view.copy().is_sensitive());
    assert!(view.save().is_sensitive());
    let text = view
        .report()
        .buffer()
        .text(
            &view.report().buffer().start_iter(),
            &view.report().buffer().end_iter(),
            false,
        )
        .to_string();
    assert_eq!(Some(text.as_str()), completed.presentation().report_text());

    let buffer = view.report().buffer();
    let start = buffer.start_iter();
    let mut bound = start;
    bound.forward_chars(6);
    buffer.select_range(&start, &bound);
    assert!(buffer.selection_bounds().is_some());
    view.render(&wide_window, completed.presentation());
    assert!(
        buffer.selection_bounds().is_some(),
        "an unchanged report must preserve the user's selection"
    );

    // Native dismissal emits Close. Feeding that intent back through the
    // application and rendering its transition must not emit a second Close.
    intents.borrow_mut().clear();
    gtk::prelude::GtkWindowExt::set_focus(&wide_window, None::<&gtk::Widget>);
    view.dialog.close();
    pump_until(|| intents.borrow().contains(&DiagnosticsIntent::Close));
    assert_eq!(*intents.borrow(), vec![DiagnosticsIntent::Close]);
    let closed = model
        .handle_intent(DiagnosticsIntent::Close)
        .expect("native close transition");
    intents.borrow_mut().clear();
    view.render(&wide_window, closed.presentation());
    pump_until(|| !view.presented.get() && !view.suppress_close.get());
    assert!(intents.borrow().is_empty());
    assert_eq!(
        gtk::prelude::GtkWindowExt::focus(&wide_window).as_ref(),
        Some(origin.upcast_ref()),
        "closing diagnostics restores keyboard focus after a native chooser",
    );
    wide_window.close();

    let narrow_window = adw::ApplicationWindow::builder()
        .application(application)
        .default_width(350)
        .default_height(600)
        .build();
    narrow_window.present();
    let reopened = model
        .handle_intent(DiagnosticsIntent::Open)
        .expect("reopening diagnostics");
    view.render(&narrow_window, reopened.presentation());
    pump_until(|| {
        let positions = action_row_positions(&view.actions);
        view.presented.get()
            && view.report().width() > 0
            && view.report().width() < 350
            && positions.len() == 4
            && positions.iter().any(|position| *position != positions[0])
    });
    assert!(view.presented.get());
    assert!(view.report().width() > 0);
    assert!(view.report().width() < 350);
    let narrow_action_rows = action_row_positions(&view.actions);
    assert_eq!(narrow_action_rows.len(), 4);
    assert!(narrow_action_rows
        .iter()
        .any(|position| *position != narrow_action_rows[0]));
    let reopened_completed = model
        .complete_read(
            reopened.read_operation().unwrap(),
            Ok(DiagnosticsReport::new(
                1_001,
                vec![DiagnosticSection::new("Application", "safe fixture")],
            )),
        )
        .unwrap();
    view.render(&narrow_window, reopened_completed.presentation());

    let refresh = model
        .handle_intent(DiagnosticsIntent::Refresh)
        .expect("refresh transition");
    view.render(&narrow_window, refresh.presentation());
    assert_eq!(view.status.text(), "Collecting diagnostics…");
    assert!(!view.refresh().is_sensitive());
    let failed = model
        .complete_read(
            refresh.read_operation().unwrap(),
            Err(DiagnosticsError::collection_stopped()),
        )
        .unwrap();
    view.render(&narrow_window, failed.presentation());
    assert!(view.status.is_visible());
    assert_eq!(
        view.status.text(),
        "Collection stopped unexpectedly. Refresh to try again."
    );
    assert_eq!(view.status.accessible_role(), gtk::AccessibleRole::Alert);
    assert!(view.copy().is_sensitive());
    assert!(view.save().is_sensitive());

    let choosing = model
        .handle_intent(DiagnosticsIntent::Save)
        .expect("save chooser transition");
    view.render(&narrow_window, choosing.presentation());
    let saving = model
        .handle_intent(DiagnosticsIntent::SaveDestination {
            request: choosing.save_request().unwrap(),
            path: Some(PathBuf::from("diagnostics.txt")),
        })
        .expect("save transition");
    view.render(&narrow_window, saving.presentation());
    assert_eq!(view.status.text(), "Saving diagnostics…");
    assert!(!view.refresh().is_sensitive());
    assert!(!view.copy().is_sensitive());
    let saved = model
        .complete_save(saving.save_operation().unwrap(), Ok(()))
        .unwrap();
    view.render(&narrow_window, saved.presentation());

    intents.borrow_mut().clear();
    view.refresh().emit_clicked();
    view.copy().emit_clicked();
    view.save().emit_clicked();
    assert_eq!(
        *intents.borrow(),
        vec![
            DiagnosticsIntent::Refresh,
            DiagnosticsIntent::Copy,
            DiagnosticsIntent::Save,
        ]
    );

    // Application-driven dismissal is asynchronous in libadwaita. Its
    // closed signal must be suppressed, and the same widget must reopen once
    // the close callback has completed.
    intents.borrow_mut().clear();
    let hidden = model
        .handle_intent(DiagnosticsIntent::Close)
        .expect("application close transition");
    view.render(&narrow_window, hidden.presentation());
    pump_until(|| !view.presented.get() && !view.suppress_close.get());
    assert!(intents.borrow().is_empty());
    assert!(!view.presented.get());

    let reopened = model
        .handle_intent(DiagnosticsIntent::Open)
        .expect("reopen after application close");
    view.render(&narrow_window, reopened.presentation());
    pump_until(|| view.presented.get() && view.report().width() > 0);
    assert!(view.presented.get());

    let hidden = model
        .handle_intent(DiagnosticsIntent::Close)
        .expect("final close transition");
    view.render(&narrow_window, hidden.presentation());
    pump_until(|| !view.presented.get() && !view.suppress_close.get());
    narrow_window.close();
}
