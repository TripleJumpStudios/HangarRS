//! The tab set, as a plain enum.
//!
//! Eight rail destinations, with loadouts and advanced tools as local views.
//! A crate that manages docking and reordering would be more code than the thing it replaces, and
//! neither behaviour is wanted here.

use egui_phosphor::regular as icons;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Tab {
    Preflight,
    Hangar,
    Profiles,
    Install,
    Bindings,
    Map,
    Bridges,
    Console,
    Settings,
    Prefix,
}

/// Nav rail group one.
pub const PRIMARY_TABS: [Tab; 2] = [Tab::Preflight, Tab::Map];

/// Nav rail group two.
pub const SECONDARY_TABS: [Tab; 4] = [Tab::Hangar, Tab::Install, Tab::Bindings, Tab::Bridges];
pub const UTILITY_TABS: [Tab; 2] = [Tab::Console, Tab::Settings];

/// Remember each workspace's local view when navigating elsewhere.
pub struct Navigation {
    hangar: Tab,
    settings: Tab,
}

impl Default for Navigation {
    fn default() -> Self {
        Self {
            hangar: Tab::Hangar,
            settings: Tab::Settings,
        }
    }
}

impl Navigation {
    pub fn remember(&mut self, tab: Tab) {
        match tab.parent() {
            Tab::Hangar => self.hangar = tab,
            Tab::Settings => self.settings = tab,
            _ => {}
        }
    }

    pub fn destination(&self, tab: Tab) -> Tab {
        match tab {
            Tab::Hangar => self.hangar,
            Tab::Settings => self.settings,
            _ => tab,
        }
    }
}

#[cfg(test)]
pub const ALL_TABS: [Tab; 10] = [
    Tab::Preflight,
    Tab::Map,
    Tab::Hangar,
    Tab::Profiles,
    Tab::Install,
    Tab::Bindings,
    Tab::Bridges,
    Tab::Console,
    Tab::Settings,
    Tab::Prefix,
];

impl Tab {
    pub fn parent(self) -> Self {
        match self {
            Self::Profiles => Self::Hangar,
            Self::Prefix => Self::Settings,
            other => other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tab::Preflight => "Flight",
            Tab::Hangar => "Hangar",
            Tab::Profiles => "Loadouts",
            Tab::Install => "Install",
            Tab::Bindings => "Controls",
            Tab::Map => "Map",
            Tab::Bridges => "Connections",
            Tab::Console => "Activity",
            Tab::Settings => "Settings",
            Tab::Prefix => "Advanced",
        }
    }

    /// Phosphor glyph. Always rendered through the `Icons` font family — see
    /// `hangar_ui::theme::ICONS` for why the family matters.
    pub fn icon(self) -> &'static str {
        match self {
            Tab::Preflight => icons::ROCKET_LAUNCH,
            Tab::Hangar => icons::PACKAGE,
            Tab::Profiles => icons::LIST_CHECKS,
            Tab::Install => icons::DOWNLOAD_SIMPLE,
            Tab::Bindings => icons::GAME_CONTROLLER,
            Tab::Map => icons::MAP_TRIFOLD,
            Tab::Bridges => icons::PLUGS_CONNECTED,
            Tab::Console => icons::TERMINAL_WINDOW,
            Tab::Settings => icons::GEAR_SIX,
            Tab::Prefix => icons::WRENCH,
        }
    }
}
