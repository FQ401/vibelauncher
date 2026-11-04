//! Top-level sections of the left rail.

use crate::strings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Section {
    #[default]
    Home,
    Instances,
    Catalog,
    Account,
    Settings,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Home,
        Section::Instances,
        Section::Catalog,
        Section::Account,
        Section::Settings,
    ];

    /// The section with the given lowercase name (`home`, `instances`, ...).
    pub fn from_name(name: &str) -> Option<Section> {
        match name {
            "home" => Some(Section::Home),
            "instances" => Some(Section::Instances),
            "catalog" => Some(Section::Catalog),
            "account" => Some(Section::Account),
            "settings" => Some(Section::Settings),
            _ => None,
        }
    }

    /// The message id of the section's name.
    pub fn label_id(self) -> &'static str {
        match self {
            Section::Home => strings::NAV_HOME,
            Section::Instances => strings::NAV_INSTANCES,
            Section::Catalog => strings::NAV_CATALOG,
            Section::Account => strings::NAV_ACCOUNT,
            Section::Settings => strings::NAV_SETTINGS,
        }
    }

    /// The slot of the section in [`Section::ALL`] (the per-section memory is indexed by it).
    pub fn slot(self) -> usize {
        match self {
            Section::Home => 0,
            Section::Instances => 1,
            Section::Catalog => 2,
            Section::Account => 3,
            Section::Settings => 4,
        }
    }
}

/// A page of the launcher (N1): level 1 is a section root, deeper levels open inside sections.
/// Tabs are a state of a page and never a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// The root of a section (page depth 1).
    Root(Section),
}

impl Page {
    /// The section the page belongs to.
    pub fn section(self) -> Section {
        match self {
            Page::Root(section) => section,
        }
    }
}

/// The page history and the per-section memory (N4): the history holds pages only, a section
/// remembers its last page, and walking back (Alt+←, the mouse back button, Esc past the open
/// layers) returns to the previous page. Going to a page also makes it the section's last one.
#[derive(Debug)]
pub struct Router {
    /// The open page.
    page: Page,
    /// The pages to return to, newest last.
    back: Vec<Page>,
    /// The last open page of each section, for the rail's first click.
    last: [Page; 5],
}

impl Default for Router {
    fn default() -> Self {
        let mut last = [Page::Root(Section::Home); 5];
        for section in Section::ALL {
            last[section.slot()] = Page::Root(section);
        }
        Self {
            page: Page::Root(Section::default()),
            back: Vec::new(),
            last,
        }
    }
}

impl Router {
    /// The open page.
    pub fn page(&self) -> Page {
        self.page
    }

    /// The section of the open page.
    pub fn section(&self) -> Section {
        self.page.section()
    }

    /// Opens a page: remembers it as its section's last page and pushes the current one on the
    /// back stack. Opening the current page is a no-op.
    pub fn open(&mut self, page: Page) {
        if page == self.page {
            return;
        }
        self.back.push(self.page);
        self.remember(page);
    }

    /// The rail's click (N4): the first click opens the section's last page, a repeat click on
    /// the open page goes to the section's root.
    pub fn rail(&mut self, section: Section) {
        let page = self.last[section.slot()];
        if page == self.page {
            self.open(Page::Root(section));
        } else {
            self.open(page);
        }
    }

    /// Goes back one page; returns false when the history is empty.
    pub fn back(&mut self) -> bool {
        match self.back.pop() {
            Some(page) => {
                self.remember(page);
                true
            }
            None => false,
        }
    }

    /// Sets the page and remembers it as its section's last page.
    fn remember(&mut self, page: Page) {
        self.page = page;
        self.last[page.section().slot()] = page;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_history_holds_pages_and_the_rail_remembers_the_last_one() {
        let mut router = Router::default();
        assert_eq!(router.page(), Page::Root(Section::Home));
        // The rail opens the section's root and remembers it as the last page.
        router.rail(Section::Catalog);
        assert_eq!(router.page(), Page::Root(Section::Catalog));
        // A repeat click on the open section is a no-op at the root.
        router.rail(Section::Catalog);
        assert_eq!(router.page(), Page::Root(Section::Catalog));
        // Another section stacks the current page for going back.
        router.rail(Section::Settings);
        assert_eq!(router.page(), Page::Root(Section::Settings));
        assert!(router.back());
        assert_eq!(router.page(), Page::Root(Section::Catalog));
        assert!(router.back());
        assert_eq!(router.page(), Page::Root(Section::Home));
        assert!(!router.back(), "the history is empty");
    }
}
