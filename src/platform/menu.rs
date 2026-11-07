//! The application menu (macOS menu bar; the window's menu bar on Windows and
//! Linux, whose shortcuts also work while the title bar is hidden).
//!
//! On macOS the first submenu is the app menu (AppKit titles it with the app's
//! name): About, Settings…, Check for updates, Hide/Hide Others/Show All, Quit.
//! The other menus are the same on every desktop: File, Edit, View, Window, Help.
//!
//! Items are rebuilt when the language, the destination or whether a send can
//! start changes — not on every transfer-progress tick — and their handlers
//! call [`Session`] methods, so the menu drives the same commands as the views.

use crate::i18n::{self, Language};
use crate::model::{Session, View};
use crate::platform::{files, quit};
use nsh_core::desktop::MenuRole;
use nsh_core::element::desktop::{MenuItem, order_front, set_application_menu};
use nsh_core::platform::Os;
use nsh_core::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

/// LocalSend's website, as the Help menu opens it.
pub const WEBSITE: &str = "https://localsend.org";
/// This app's source repository, as the Help menu opens it.
pub const SOURCE: &str = crate::update::REPOSITORY_URL;

/// Installs the application menu, and keeps it current.
///
/// Must run inside a component body (the root component calls it through
/// [`crate::platform::install`]): it creates an effect.
pub fn install(session: Session) {
    if !is_desktop() {
        return;
    }
    // The effect itself re-runs on every revision bump (a progress event
    // subscribes it to the whole `read()` revision); rebuilding the native
    // menu is what is expensive, so only these three inputs re-set it.
    let last: Rc<Cell<Option<(Language, View, bool)>>> = Rc::new(Cell::new(None));
    effect(move || {
        let language = session.language.get();
        let (view, can_send) = session.read(|state| (state.view, state.can_send()));
        let current = Some((language, view, can_send));
        if last.get() == current {
            return;
        }
        last.set(current);
        set_application_menu(menus_at(&session, language, view, can_send));
    });
}

/// The menu's items, for [`set_application_menu`] and for a custom title bar's
/// menu button ([`nsh_core::element::desktop::popup_menu_at`]).
///
/// Reads the session, so inside an effect it stays current.
pub fn menus(session: &Session) -> Vec<MenuItem> {
    let language = session.language.get();
    let (view, can_send) = session.read(|state| (state.view, state.can_send()));
    menus_at(session, language, view, can_send)
}

fn menus_at(session: &Session, language: Language, view: View, can_send: bool) -> Vec<MenuItem> {
    let mac = os() == Os::MacOs;
    let mut menus = Vec::with_capacity(6);
    if mac {
        menus.push(app_menu(session, language));
    }
    menus.push(file_menu(session, language, mac, can_send));
    menus.push(edit_menu(language));
    menus.push(view_menu(session, language, view));
    menus.push(window_menu(language));
    menus.push(help_menu(session, language, mac));
    menus
}

/// The app menu: About, Settings…, Check for updates, Hide/Show and Quit
/// (macOS only; the platform leaves About/Hide out elsewhere).
fn app_menu(session: &Session, language: Language) -> MenuItem {
    let settings = session.clone();
    let quit_app = session.clone();
    MenuItem::submenu(
        text(language, "app.name"),
        vec![
            MenuItem::role(MenuRole::About).label(text(language, "menu.about")),
            MenuItem::separator(),
            MenuItem::new(text(language, "menu.settings"))
                .shortcut("mod+,")
                .on_select(move || settings.open_settings(true)),
            check_for_updates_item(session, language),
            MenuItem::separator(),
            MenuItem::role(MenuRole::Hide).label(text(language, "menu.hide")),
            MenuItem::role(MenuRole::HideOthers).label(text(language, "menu.hide_others")),
            MenuItem::role(MenuRole::ShowAll).label(text(language, "menu.show_all")),
            MenuItem::separator(),
            // A custom item, not `MenuRole::Quit`: quitting goes through
            // `quit::quit` so an active transfer is confirmed and the session
            // is flushed first (INFRA-09). The role would terminate directly.
            MenuItem::new(text(language, "menu.quit"))
                .shortcut("mod+q")
                .on_select(move || quit::quit(&quit_app)),
        ],
    )
}

/// The File menu: pick files and folders, send the composer, close the window.
fn file_menu(session: &Session, language: Language, mac: bool, can_send: bool) -> MenuItem {
    let choose_files = session.clone();
    let choose_folder = session.clone();
    let send = session.clone();
    let close = session.clone();
    let settings = session.clone();
    let quit_app = session.clone();
    let mut items = vec![
        MenuItem::new(text(language, "menu.choose_files"))
            .shortcut("mod+o")
            .on_select(move || files::choose_files(&choose_files)),
        MenuItem::new(text(language, "menu.choose_folder"))
            .shortcut("mod+shift+o")
            .on_select(move || files::choose_folder(&choose_folder)),
        MenuItem::separator(),
        MenuItem::new(text(language, "nav.send"))
            .shortcut("mod+enter")
            .disabled(!can_send)
            .on_select(move || send.send()),
        MenuItem::separator(),
        MenuItem::new(text(language, "menu.close_window"))
            .shortcut("mod+w")
            .on_select(move || quit::close(&close)),
    ];
    if !mac {
        // No app menu: Settings, updates and Quit live in File on Windows/Linux.
        items.push(MenuItem::separator());
        items.push(
            MenuItem::new(text(language, "menu.settings"))
                .shortcut("mod+,")
                .on_select(move || settings.open_settings(true)),
        );
        items.push(check_for_updates_item(session, language));
        // Custom, like the macOS app menu's Quit: confirm + flush first.
        items.push(
            MenuItem::new(text(language, "menu.quit"))
                .shortcut("mod+q")
                .on_select(move || quit::quit(&quit_app)),
        );
    }
    MenuItem::submenu(text(language, "menu.file"), items)
}

/// Show the existing update UI and start the same manual check as its button.
fn check_for_updates_item(session: &Session, language: Language) -> MenuItem {
    let session = session.clone();
    MenuItem::new(text(language, "update.check")).on_select(move || {
        order_front();
        session.open_settings(true);
        session.check_for_updates(true);
    })
}

/// The Edit menu: the platform's text-editing items, with localized labels.
fn edit_menu(language: Language) -> MenuItem {
    MenuItem::submenu(
        text(language, "menu.edit"),
        vec![
            MenuItem::role(MenuRole::Undo).label(text(language, "menu.undo")),
            MenuItem::role(MenuRole::Redo).label(text(language, "menu.redo")),
            MenuItem::separator(),
            MenuItem::role(MenuRole::Cut).label(text(language, "menu.cut")),
            MenuItem::role(MenuRole::Copy).label(text(language, "menu.copy")),
            MenuItem::role(MenuRole::Paste).label(text(language, "menu.paste")),
            MenuItem::separator(),
            MenuItem::role(MenuRole::SelectAll).label(text(language, "menu.select_all")),
        ],
    )
}

/// The View menu: the three destinations, restarting the network, full screen.
fn view_menu(session: &Session, language: Language, view: View) -> MenuItem {
    let send = session.clone();
    let receive = session.clone();
    let history = session.clone();
    let restart = session.clone();
    MenuItem::submenu(
        text(language, "menu.view"),
        vec![
            MenuItem::new(text(language, "nav.send"))
                .shortcut("mod+1")
                .checked(view == View::Send)
                .on_select(move || send.set_view(View::Send)),
            MenuItem::new(text(language, "nav.receive"))
                .shortcut("mod+2")
                .checked(view == View::Receive)
                .on_select(move || receive.set_view(View::Receive)),
            MenuItem::new(text(language, "nav.history"))
                .shortcut("mod+3")
                .checked(view == View::History)
                .on_select(move || history.set_view(View::History)),
            MenuItem::separator(),
            MenuItem::new(text(language, "menu.restart_network"))
                .shortcut("mod+r")
                .on_select(move || restart.restart_network()),
            MenuItem::separator(),
            MenuItem::role(MenuRole::ToggleFullScreen)
                .label(text(language, "menu.toggle_full_screen")),
        ],
    )
}

/// The Window menu: Minimize, Zoom and Bring All to Front.
fn window_menu(language: Language) -> MenuItem {
    MenuItem::submenu(
        text(language, "menu.window"),
        vec![
            MenuItem::role(MenuRole::Minimize).label(text(language, "menu.minimize")),
            MenuItem::role(MenuRole::Zoom).label(text(language, "menu.zoom")),
            MenuItem::separator(),
            MenuItem::new(text(language, "menu.bring_all_to_front")).on_select(order_front),
        ],
    )
}

/// The Help menu: the website and the source repository.
fn help_menu(session: &Session, language: Language, mac: bool) -> MenuItem {
    let website = session.clone();
    let source = session.clone();
    let about = session.clone();
    let mut items = Vec::new();
    if !mac {
        items.push(
            MenuItem::new(text(language, "menu.about"))
                .on_select(move || about.open_settings(true)),
        );
        items.push(MenuItem::separator());
    }
    items.push(
        MenuItem::new(text(language, "menu.website"))
            .on_select(move || open_url_in(&website, WEBSITE)),
    );
    items.push(
        MenuItem::new(text(language, "menu.source"))
            .on_select(move || open_url_in(&source, SOURCE)),
    );
    MenuItem::submenu(text(language, "menu.help"), items)
}

/// Opens `url` in the default browser, in a task owned by the session.
fn open_url_in(session: &Session, url: &'static str) {
    session.task_detached(async move {
        // `false` means the system refused to open it (no handler); there is no
        // useful recovery, and the system already told the user.
        let _ = open_url(url).await;
    });
}

/// `key` in `language`, as the String a [`MenuItem`] label needs.
fn text(language: Language, key: &'static str) -> String {
    i18n::text(language, key).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_menu_key_is_translated() {
        for language in Language::ALL {
            for key in [
                "menu.file",
                "menu.edit",
                "menu.view",
                "menu.window",
                "menu.help",
                "menu.about",
                "menu.settings",
                "menu.hide",
                "menu.hide_others",
                "menu.show_all",
                "menu.quit",
                "menu.choose_files",
                "menu.choose_folder",
                "menu.close_window",
                "menu.undo",
                "menu.redo",
                "menu.cut",
                "menu.copy",
                "menu.paste",
                "menu.select_all",
                "menu.restart_network",
                "menu.toggle_full_screen",
                "menu.minimize",
                "menu.zoom",
                "menu.bring_all_to_front",
                "menu.website",
                "menu.source",
            ] {
                assert_ne!(i18n::text(language, key), "<missing>", "{key} is missing");
            }
        }
    }
}
