//! The settings sheet.
//!
//! Per the design, there is no Save button: every control applies through
//! [`Session::update_settings`] immediately. Text fields apply on Return or
//! when they lose focus, so typing an alias does not restart the network on
//! every keystroke (the model restarts it when the alias changes).
//!
//! The shell owns the sheet's chrome (card, width, padding, close button and
//! scrim): this module returns only the inner content.

use crate::i18n::Language;
use crate::model::Session;
use crate::model::UpdateStatus;
use crate::model::updater::{failure_key, manual_key};
use crate::net::DeviceType;
use crate::ui::{
    Appearance, ButtonKind, IconName, button, colors, control, radius, segmented, space, text_size,
    toggle,
};
use nsh_core::element::input;
use nsh_core::prelude::*;

use super::send::fill;

/// Which device types the picker offers, in order.
const DEVICE_TYPES: [DeviceType; 4] = [
    DeviceType::Desktop,
    DeviceType::Mobile,
    DeviceType::Headless,
    DeviceType::Server,
];

fn device_type_key(device_type: DeviceType) -> &'static str {
    match device_type {
        DeviceType::Desktop => "device.type.desktop",
        DeviceType::Mobile => "device.type.mobile",
        DeviceType::Web => "device.type.web",
        DeviceType::Headless => "device.type.headless",
        DeviceType::Server => "device.type.server",
        DeviceType::Unknown => "device.type.unknown",
    }
}

#[derive(Clone, Default)]
struct SettingsUi {
    /// The alias being typed (`None`: show the stored one).
    alias: Option<String>,
    /// The PIN being typed (`None`: show the stored one).
    pin: Option<String>,
}

fn ui() -> Signal<SettingsUi> {
    global::<SettingsUi>()
}

/// The settings sheet content.
///
/// The whole content is built in a `dynamic` region so a language change
/// rebuilds the section titles and picker labels (built, unlike values, once).
pub fn settings_sheet(session: Session, settings_trigger: ElementRef) -> impl IntoElement {
    let drafts = ui();
    // The sheet unmounts when it closes: drop uncommitted drafts so opening it
    // again shows the stored values, not what was left in a field, and hand
    // the keyboard focus back to the row that opened the sheet.
    let sheet_still_open = session.clone();
    cleanup(move || {
        drafts.update(|state| {
            state.alias = None;
            state.pin = None;
        });
        // Only hand the focus back when no other sheet took over (a PIN
        // request can unmount settings while its own sheet opens).
        let another_sheet = sheet_still_open.pin_request.get_untracked().is_some()
            || sheet_still_open.read_transfers(|state| state.incoming.is_some());
        if !another_sheet {
            settings_trigger.focus();
        }
    });
    dynamic(move || {
        view()
            .test_id("settings-sheet")
            .w_full()
            .flex_col()
            .gap(px(space::LG))
            .child(general_section(session.clone()))
            .child(receiving_section(session.clone()))
            .child(appearance_section(session.clone()))
            .child(language_section(session.clone()))
            .child(about_section(session.clone()))
    })
}

// ---------------------------------------------------------------------------
// General
// ---------------------------------------------------------------------------

/// The note under a network setting while a transfer is active: the change
/// is stored at once but only reaches the network when the transfer ends, and
/// hiding that would read as a setting that did nothing.
fn applies_after_transfer(session: &Session) -> AnyElement {
    let busy = computed({
        let session = (*session).clone();
        move || {
            session
                .read_transfers(|state| state.transfers.iter().any(|transfer| transfer.is_active()))
        }
    });
    let note = (*session).clone();
    when(
        move || busy.get(),
        move || {
            let note = note.clone();
            text(move || note.text("settings.applies_after_transfer"))
                .text_size(px(text_size::CAPTION))
                .text_color(colors::WARNING)
                .number_of_lines(2)
        },
    )
    .into_element()
}

fn general_section(session: Session) -> impl IntoElement {
    let type_selector = session.clone();
    let discoverable = session.clone();
    let discoverable_label = session.clone();
    let discoverable_change = session.clone();
    let encrypt = session.clone();
    let encrypt_label = session.clone();
    let encrypt_change = session.clone();
    let run_background = session.clone();
    let run_background_label = session.clone();
    let run_background_change = session.clone();
    let device_type_options: Vec<SharedString> = DEVICE_TYPES
        .iter()
        .map(|device_type| session.text(device_type_key(*device_type)))
        .collect();
    let device_type_selected = move || {
        type_selector.read_settings(|state| {
            DEVICE_TYPES
                .iter()
                .position(|device_type| *device_type == state.settings.device_type)
                .unwrap_or(0)
        })
    };
    let type_change = session.clone();
    section(
        session.text("settings.general"),
        vec![
            row(
                session.text("settings.name"),
                SharedString::default(),
                alias_field(&session),
                Some(applies_after_transfer(&session)),
            ),
            row(
                session.text("settings.device_type"),
                SharedString::default(),
                segmented(device_type_options)
                    .selected(device_type_selected)
                    .on_change(move |index| {
                        let device_type = DEVICE_TYPES
                            .get(index)
                            .copied()
                            .unwrap_or(DeviceType::Desktop);
                        type_change.update_settings(|settings| settings.device_type = device_type);
                    })
                    .into_element(),
                // The device type changes the advertised payload, so the node
                // restarts for it like the fields above.
                Some(applies_after_transfer(&session)),
            ),
            row(
                session.text("settings.discoverable"),
                session.text("settings.discoverable_hint"),
                toggle(move || discoverable.read_settings(|state| state.settings.discoverable))
                    .label(discoverable_label.text("settings.discoverable"))
                    .on_change(move |value| {
                        discoverable_change
                            .update_settings(|settings| settings.discoverable = value);
                    })
                    .into_element(),
                Some(applies_after_transfer(&session)),
            ),
            row(
                session.text("settings.https"),
                session.text("settings.https_hint"),
                toggle(move || encrypt.read_settings(|state| state.settings.https))
                    .label(encrypt_label.text("settings.https"))
                    .on_change(move |value| {
                        encrypt_change.update_settings(|settings| settings.https = value);
                    })
                    .into_element(),
                Some(applies_after_transfer(&session)),
            ),
            row(
                session.text("settings.run_background"),
                session.text("settings.run_background_hint"),
                toggle(move || {
                    run_background.read_settings(|state| state.settings.run_in_background)
                })
                .label(run_background_label.text("settings.run_background"))
                .on_change(move |value| {
                    run_background_change
                        .update_settings(|settings| settings.run_in_background = value);
                })
                .into_element(),
                None,
            ),
        ],
    )
}

fn alias_field(session: &Session) -> AnyElement {
    let ui = ui();
    let this = (*session).clone();
    let submit = (*session).clone();
    let blur = (*session).clone();
    input()
        .test_id("settings-alias")
        // The sheet is modal: its first control takes focus when it opens.
        .auto_focus(true)
        .value(move || {
            ui.get()
                .alias
                .unwrap_or_else(|| this.read_settings(|state| state.settings.alias.clone()))
        })
        .on_change(move |text: &str| ui.update(|state| state.alias = Some(text.to_owned())))
        .on_submit(move |text: &str| apply_alias(&submit, ui, text))
        .on_blur(move || {
            let value = ui.get_untracked().alias.clone().unwrap_or_default();
            apply_alias(&blur, ui, &value);
        })
        .max_length(80)
        .w(px(control::FIELD_ALIAS))
        .h(px(28.0))
        .px(px(space::SM))
        .rounded(px(radius::CONTROL))
        .bg(colors::FIELD)
        .border_1()
        .border_color(colors::BORDER)
        .text_size(px(text_size::BODY))
        .into_element()
}

fn apply_alias(session: &Session, ui: Signal<SettingsUi>, value: &str) {
    let value = value.trim().to_string();
    let changed = !value.is_empty() && session.read_settings(|state| state.settings.alias != value);
    if changed {
        session.update_settings(|settings| settings.alias = value);
        session.toast(session.text("settings.saved"));
    }
    ui.update(|state| state.alias = None);
}

// ---------------------------------------------------------------------------
// Receiving
// ---------------------------------------------------------------------------

fn receiving_section(session: Session) -> impl IntoElement {
    let folder = session.clone();
    let folder_button = session.clone();
    let quick_save = session.clone();
    let quick_save_label = session.clone();
    let quick_save_change = session.clone();
    let favorites_only = session.clone();
    let favorites_only_label = session.clone();
    let favorites_only_change = session.clone();
    section(
        session.text("settings.receiving"),
        vec![
            row(
                session.text("receive.downloads"),
                SharedString::default(),
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(space::SM))
                    .child(
                        text(move || {
                            folder.read_settings(|state| {
                                state.settings.downloads.display().to_string()
                            })
                        })
                        .text_size(px(text_size::SMALL))
                        .text_color(colors::MUTED)
                        .number_of_lines(1)
                        .text_overflow(nsh_core::EllipsizeMode::Middle)
                        .max_w(px(control::FIELD_ALIAS)),
                    )
                    .child(
                        button(session.text("settings.open_downloads"))
                            .kind(ButtonKind::Ghost)
                            .small()
                            .on_press({
                                let opener = session.clone();
                                move || opener.open_downloads()
                            }),
                    )
                    .child(
                        button(session.text("receive.change"))
                            .kind(ButtonKind::Ghost)
                            .small()
                            .on_press(move || folder_button.choose_downloads_dir()),
                    )
                    .into_element(),
                Some(applies_after_transfer(&session)),
            ),
            row(
                session.text("receive.quick_save"),
                session.text("receive.quick_save_hint"),
                toggle(move || quick_save.read_settings(|state| state.settings.quick_save))
                    .label(quick_save_label.text("receive.quick_save"))
                    .on_change(move |enabled| {
                        quick_save_change.update_settings(|settings| settings.quick_save = enabled);
                    })
                    .into_element(),
                None,
            ),
            row(
                session.text("receive.quick_save_favorites"),
                session.text("receive.quick_save_favorites_hint"),
                toggle(move || {
                    favorites_only.read_settings(|state| state.settings.quick_save_favorites_only)
                })
                .label(favorites_only_label.text("receive.quick_save_favorites"))
                .on_change(move |enabled| {
                    favorites_only_change
                        .update_settings(|settings| settings.quick_save_favorites_only = enabled);
                })
                .into_element(),
                None,
            ),
            row(
                session.text("receive.pin"),
                session.text("receive.pin_hint"),
                pin_field(&session),
                Some(applies_after_transfer(&session)),
            ),
        ],
    )
}

fn pin_field(session: &Session) -> AnyElement {
    let ui = ui();
    let this = (*session).clone();
    let submit = (*session).clone();
    let blur = (*session).clone();
    input()
        .test_id("settings-pin")
        .value(move || {
            ui.get().pin.unwrap_or_else(|| {
                this.read_settings(|state| state.settings.pin.clone().unwrap_or_default())
            })
        })
        .on_change(move |text: &str| ui.update(|state| state.pin = Some(text.to_owned())))
        .placeholder(session.text("pin.placeholder").to_string())
        .on_submit(move |text: &str| apply_pin(&submit, ui, text))
        .on_blur(move || {
            let value = ui.get_untracked().pin.clone().unwrap_or_default();
            apply_pin(&blur, ui, &value);
        })
        .max_length(12)
        .w(px(control::FIELD_PIN))
        .h(px(28.0))
        .px(px(space::SM))
        .rounded(px(radius::CONTROL))
        .bg(colors::FIELD)
        .border_1()
        .border_color(colors::BORDER)
        .text_size(px(text_size::BODY))
        .into_element()
}

fn apply_pin(session: &Session, ui: Signal<SettingsUi>, value: &str) {
    let value = value.trim().to_string();
    if !value.is_empty() && value.chars().count() < crate::model::state::MIN_PIN_CHARS {
        let unchanged =
            session.read_settings(|state| state.settings.pin.as_deref() == Some(&value));
        if !unchanged {
            session.fail(
                session
                    .text("error.pin_too_short")
                    .replace("{count}", &crate::model::state::MIN_PIN_CHARS.to_string()),
            );
            ui.update(|state| state.pin = None);
            return;
        }
    }
    let pin = if value.is_empty() { None } else { Some(value) };
    if session.read_settings(|state| state.settings.pin != pin) {
        session.update_settings(|settings| settings.pin = pin);
        session.toast(session.text("settings.saved"));
    }
    ui.update(|state| state.pin = None);
}

// ---------------------------------------------------------------------------
// Appearance and language
// ---------------------------------------------------------------------------

fn appearance_section(session: Session) -> impl IntoElement {
    let options: Vec<SharedString> = Appearance::ALL
        .iter()
        .map(|appearance| session.text(appearance.label_key()))
        .collect();
    let selector = session.clone();
    let selected = move || selector.read_settings(|state| state.settings.appearance.index());
    let change = session.clone();
    section(
        session.text("settings.appearance"),
        vec![
            view()
                .px(px(space::MD))
                .py(px(space::SM + space::XS))
                .bg(colors::INSET)
                .child(
                    segmented(options)
                        .selected(selected)
                        .on_change(move |index| {
                            let appearance =
                                Appearance::ALL.get(index).copied().unwrap_or_default();
                            change.update_settings(|settings| settings.appearance = appearance);
                        }),
                )
                .into_element(),
        ],
    )
}

fn language_section(session: Session) -> impl IntoElement {
    let options: Vec<SharedString> = Language::ALL
        .iter()
        .map(|language| SharedString::from(language.label()))
        .collect();
    let selector = session.clone();
    let selected = move || {
        selector.read_settings(|state| {
            Language::ALL
                .iter()
                .position(|language| *language == state.settings.language)
                .unwrap_or(0)
        })
    };
    let change = session.clone();
    section(
        session.text("settings.language"),
        vec![
            view()
                .px(px(space::MD))
                .py(px(space::SM + space::XS))
                .bg(colors::INSET)
                .child(
                    segmented(options)
                        .selected(selected)
                        .on_change(move |index| {
                            let language = Language::ALL.get(index).copied().unwrap_or_default();
                            change.update_settings(|settings| settings.language = language);
                        }),
                )
                .into_element(),
        ],
    )
}

// ---------------------------------------------------------------------------
// About
// ---------------------------------------------------------------------------

fn about_section(session: Session) -> impl IntoElement {
    let version = fill(
        &session.text("settings.version"),
        &[("version", session.version().to_string())],
    );
    let protocol = fill(
        &session.text("settings.protocol"),
        &[("version", session.protocol_version().to_string())],
    );
    let github = session.clone();
    let auto_update = session.clone();
    let auto_update_label = session.clone();
    let auto_update_change = session.clone();
    section(
        session.text("settings.about"),
        vec![
            row(
                session.text("app.name"),
                version,
                button(session.text("settings.github"))
                    .kind(ButtonKind::Ghost)
                    .icon(IconName::ExternalLink)
                    .on_press(move || {
                        let session = github.clone();
                        session.task(async move {
                            let _ = open_url(crate::update::REPOSITORY_URL).await;
                        });
                    })
                    .into_element(),
                None,
            ),
            row(
                session.text("settings.auto_update"),
                session.text("settings.auto_update_hint"),
                toggle(move || auto_update.read_settings(|state| state.settings.auto_update))
                    .label(auto_update_label.text("settings.auto_update"))
                    .on_change(move |value| {
                        auto_update_change.update_settings(|settings| settings.auto_update = value);
                        if value {
                            auto_update_change.check_for_updates(false);
                        }
                    })
                    .into_element(),
                None,
            ),
            update_row(&session),
            row(
                SharedString::default(),
                protocol,
                view().into_element(),
                None,
            ),
        ],
    )
}

/// The update row: what the updater is doing, and the one action that fits
/// (check, open the release page, restart to update).
fn update_row(session: &Session) -> AnyElement {
    let status = session.update_status;
    let note_session = session.clone();
    let label_session = session.clone();
    let press_session = session.clone();
    let note = computed(move || update_note(&note_session, &status.get()));
    let has_note = computed(move || !note.get().is_empty());
    let note_element = when(
        move || has_note.get(),
        move || {
            text(move || note.get())
                .text_size(px(text_size::CAPTION))
                .text_color(colors::MUTED)
                .number_of_lines(2)
        },
    )
    .into_element();
    let action = button(move || update_action_label(&label_session, &status.get()))
        .kind(ButtonKind::Secondary)
        .small()
        .disabled(move || status.get().busy())
        .on_press(move || {
            let session = press_session.clone();
            match session.update_status.get_untracked() {
                UpdateStatus::Ready { .. } => crate::platform::quit::restart_to_update(&session),
                UpdateStatus::Manual { page, .. } => {
                    session.task_detached(async move {
                        let _ = open_url(page).await;
                    });
                }
                status if status.busy() => {}
                _ => session.check_for_updates(true),
            }
        })
        .into_element();
    row(
        session.text("settings.update"),
        SharedString::default(),
        action,
        Some(note_element),
    )
}

/// The line under "Software update".
fn update_note(session: &Session, status: &UpdateStatus) -> SharedString {
    match status {
        UpdateStatus::Idle => SharedString::default(),
        UpdateStatus::Checking => session.text("update.checking"),
        UpdateStatus::UpToDate => session.text("update.up_to_date"),
        UpdateStatus::Downloading {
            version,
            received,
            total,
        } => {
            let percent = (*received * 100).checked_div(*total).unwrap_or(0).min(100);
            fill(
                &session.text("update.downloading"),
                &[
                    ("version", version.clone()),
                    ("percent", percent.to_string()),
                ],
            )
        }
        UpdateStatus::Ready { version } => fill(
            &session.text("update.ready"),
            &[("version", version.clone())],
        ),
        UpdateStatus::Manual {
            version, reason, ..
        } => {
            let available = fill(
                &session.text("update.available"),
                &[("version", version.clone())],
            );
            format!("{available} · {}", session.text(manual_key(*reason))).into()
        }
        UpdateStatus::Installing => session.text("update.installing"),
        UpdateStatus::Failed(kind) => session.text(failure_key(*kind)),
    }
}

/// The update row's button label.
fn update_action_label(session: &Session, status: &UpdateStatus) -> SharedString {
    match status {
        UpdateStatus::Checking => session.text("update.checking"),
        UpdateStatus::Downloading { .. } => session.text("update.downloading_button"),
        UpdateStatus::Ready { .. } => session.text("update.restart"),
        UpdateStatus::Manual { .. } => session.text("update.download_page"),
        UpdateStatus::Installing => session.text("update.installing"),
        UpdateStatus::Idle | UpdateStatus::UpToDate | UpdateStatus::Failed(_) => {
            session.text("update.check")
        }
    }
}

// ---------------------------------------------------------------------------
// Layout helpers
// ---------------------------------------------------------------------------

/// A titled group of rows with hairline separators.
fn section(title: SharedString, rows: Vec<AnyElement>) -> impl IntoElement {
    view()
        .flex_col()
        .gap(px(space::XS))
        .child(
            text(title)
                .text_size(px(text_size::CAPTION))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::MUTED)
                .uppercase()
                .px(px(space::MD)),
        )
        .child(
            view()
                .flex_col()
                .gap(px(1.0))
                .rounded(px(radius::CONTROL))
                .overflow_hidden()
                .bg(colors::BORDER)
                .children(rows),
        )
}

/// One settings row: label (and hint, and an optional note) on the left, the
/// control on the right.
fn row(
    label: SharedString,
    hint: SharedString,
    control: AnyElement,
    note: Option<AnyElement>,
) -> AnyElement {
    let has_hint = !hint.is_empty();
    view()
        .flex_row()
        .items_center()
        .gap(px(space::LG))
        .px(px(space::MD))
        .py(px(space::SM + 2.0))
        .bg(colors::INSET)
        .child(
            view()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(2.0))
                .child(
                    text(label)
                        .text_size(px(text_size::BODY))
                        .font_weight(FontWeight::MEDIUM),
                )
                .child(when(has_hint, move || {
                    text(hint.clone())
                        .text_size(px(text_size::CAPTION))
                        .text_color(colors::MUTED)
                }))
                .children(note),
        )
        .child(control)
        .into_element()
}
