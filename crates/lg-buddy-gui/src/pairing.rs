//! The TV form is one segment of the shared onboarding modal.
use adw::prelude::*;
use lg_buddy::{
    config::HdmiInput, presentation::pairing::PairingPresentation, setup::gui::OnboardingIntent,
};
use std::{cell::Cell, rc::Rc};

pub(crate) struct PairingForm {
    pub root: adw::PreferencesGroup,
    address: adw::EntryRow,
    mac: adw::EntryRow,
    input: adw::ComboRow,
    suppress: Rc<Cell<bool>>,
}
impl PairingForm {
    pub fn new(on_intent: Rc<dyn Fn(OnboardingIntent)>) -> Self {
        let suppress = Rc::new(Cell::new(false));
        let address = adw::EntryRow::builder()
            .title("TV address")
            .activates_default(true)
            .build();
        let mac = adw::EntryRow::builder()
            .title("MAC address")
            .activates_default(true)
            .build();
        for (row, intent) in [
            (
                &address,
                OnboardingIntent::SetAddress as fn(String) -> OnboardingIntent,
            ),
            (&mac, OnboardingIntent::SetMac),
        ] {
            row.set_input_purpose(gtk::InputPurpose::FreeForm);
            row.connect_changed({
                let suppress = suppress.clone();
                let on_intent = on_intent.clone();
                move |row| {
                    if !suppress.get() {
                        on_intent(intent(row.text().into()));
                    }
                }
            });
            row.connect_entry_activated({
                let on_intent = on_intent.clone();
                move |_| on_intent(OnboardingIntent::Submit)
            });
        }
        let input = adw::ComboRow::builder()
            .title("HDMI input")
            .model(&gtk::StringList::new(&[
                "HDMI 1", "HDMI 2", "HDMI 3", "HDMI 4",
            ]))
            .build();
        input.connect_selected_notify({
            let suppress = suppress.clone();
            move |row| {
                if !suppress.get() {
                    if let Some(input) = [
                        HdmiInput::Hdmi1,
                        HdmiInput::Hdmi2,
                        HdmiInput::Hdmi3,
                        HdmiInput::Hdmi4,
                    ]
                    .get(row.selected() as usize)
                    {
                        on_intent(OnboardingIntent::SetInput(*input));
                    }
                }
            }
        });
        let root = adw::PreferencesGroup::builder()
            .title("TV connection")
            .build();
        root.add(&address);
        root.add(&mac);
        root.add(&input);
        Self {
            root,
            address,
            mac,
            input,
            suppress,
        }
    }
    pub fn render(&self, presentation: &PairingPresentation, editable: bool) {
        self.suppress.set(true);
        set_text(&self.address, presentation.address());
        set_text(&self.mac, presentation.mac());
        self.input.set_selected(match presentation.input() {
            HdmiInput::Hdmi1 => 0,
            HdmiInput::Hdmi2 => 1,
            HdmiInput::Hdmi3 => 2,
            HdmiInput::Hdmi4 => 3,
        });
        self.root.set_sensitive(editable);
        self.suppress.set(false);
    }
    pub fn focus(&self) {
        self.address.grab_focus();
    }
}
fn set_text(row: &adw::EntryRow, text: &str) {
    if row.text().as_str() == text {
        return;
    }
    let position = row.position();
    let selection = row.selection_bounds();
    row.set_text(text);
    let length = text.chars().count() as i32;
    if let Some((start, end)) = selection {
        row.select_region(start.min(length), end.min(length));
    } else {
        row.set_position(position.min(length));
    }
}
