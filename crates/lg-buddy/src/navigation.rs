//! The destinations available in the desktop application.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApplicationPage {
    #[default]
    Overview,
    Tvs,
}

impl ApplicationPage {
    pub const ALL: [Self; 2] = [Self::Overview, Self::Tvs];

    pub fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Tvs => "TVs",
        }
    }
}

#[derive(Debug, Default)]
pub struct Navigation {
    selected: ApplicationPage,
}

impl Navigation {
    pub fn selected(&self) -> ApplicationPage {
        self.selected
    }

    pub fn select(&mut self, page: ApplicationPage) {
        self.selected = page;
    }
}
