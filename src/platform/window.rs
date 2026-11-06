//! Window setup for the app's one window.
//!
//! The shell draws its own title bar (DESIGN.md: 50 pt, traffic lights over its
//! left edge), so the system title bar is hidden. The title follows the device
//! name — the Dock tile and the window switcher show what peers see.
//!
//! nsh 0.4 has **no** window minimum-size API (`AppOptions` has no
//! `min_size`, and `set_frame` only moves/resizes). [`setup`] therefore clamps
//! the window to [`MIN_WIDTH`]×[`MIN_HEIGHT`] through a `window()` effect: a
//! window that the user shrinks below the minimum grows back to it. The first
//! frame is 0×0 (an nsh gotcha), so it is skipped.
//!
//! nsh 0.4 also has no launch-at-login or single-instance API; the menu bar
//! item lives in [`super::status_item`].

use crate::model::Session;
use nsh_core::desktop::{TitleBarStyle, WindowFrame};
use nsh_core::element::desktop::{set_frame, set_title, set_title_bar};
use nsh_core::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// The app's own title bar's height, in points (`docs/DESIGN.md`).
pub const TITLE_BAR_HEIGHT: f32 = 50.0;

/// The narrowest the window is allowed to get, in points.
pub const MIN_WIDTH: f32 = 960.0;

/// The shortest the window is allowed to get, in points.
pub const MIN_HEIGHT: f32 = 640.0;

/// Sets up the window: hidden system title bar, a title that follows the
/// device alias, and the soft minimum size.
///
/// Must run inside a component body (the root component calls it through
/// [`crate::platform::install`]): it creates effects.
pub fn setup(session: Session) {
    if !is_desktop() {
        return;
    }
    effect(|| set_title_bar(TitleBarStyle::Hidden, Some(px(TITLE_BAR_HEIGHT))));

    let last_title = Rc::new(RefCell::new(String::new()));
    effect(move || {
        // Settings domain only: this effect must not re-run for progress.
        let alias = session.read_settings(|state| state.settings.alias.trim().to_owned());
        let title = if alias.is_empty() {
            session.text("app.name").to_string()
        } else {
            alias
        };
        if *last_title.borrow() == title {
            return;
        }
        *last_title.borrow_mut() = title.clone();
        set_title(title);
    });

    effect(|| {
        let size = window();
        let (width, height) = (size.width.0, size.height.0);
        // The first frame is 0×0; the real size follows.
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        if width >= MIN_WIDTH && height >= MIN_HEIGHT {
            return;
        }
        set_frame(WindowFrame {
            width: Some(px(width.max(MIN_WIDTH))),
            height: Some(px(height.max(MIN_HEIGHT))),
            ..WindowFrame::default()
        });
    });
}
