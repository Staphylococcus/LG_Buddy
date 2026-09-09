//! The destinations available in the desktop application.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApplicationPage {
    #[default]
    Overview,
    Tvs,
    Settings,
}

impl ApplicationPage {
    pub const ALL: [Self; 3] = [Self::Overview, Self::Tvs, Self::Settings];

    pub fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Tvs => "TVs",
            Self::Settings => "Settings",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Navigation {
    selected: ApplicationPage,
    tabs_visible: bool,
    profile_known: bool,
}

impl Default for Navigation {
    fn default() -> Self {
        Self {
            selected: ApplicationPage::Tvs,
            tabs_visible: false,
            profile_known: false,
        }
    }
}

impl Navigation {
    pub fn selected(&self) -> ApplicationPage {
        self.selected
    }

    pub fn tabs_visible(&self) -> bool {
        self.tabs_visible
    }

    pub fn select(&mut self, page: ApplicationPage) -> bool {
        if !self.tabs_visible && page != ApplicationPage::Tvs {
            return false;
        }
        self.selected = page;
        true
    }

    pub(crate) fn update_profiles(&mut self, status: &crate::presentation::tvs::TvsStatus) {
        use crate::presentation::tvs::TvsStatus;
        match status {
            TvsStatus::Empty { .. } => {
                self.selected = ApplicationPage::Tvs;
                self.tabs_visible = false;
                self.profile_known = true;
            }
            TvsStatus::Ready => {
                if !self.profile_known || !self.tabs_visible {
                    self.selected = ApplicationPage::Overview;
                }
                self.tabs_visible = true;
                self.profile_known = true;
            }
            TvsStatus::Failed(_) if !self.profile_known => {
                // An unreadable profile is not proof of a fresh installation.
                // Keep Settings accessible so invalid configuration can be fixed.
                self.tabs_visible = true;
            }
            TvsStatus::Loading { .. } | TvsStatus::Failed(_) => {}
        }
    }
}
