//! The design system's controls: buttons, switches, segmented pickers,
//! progress, device tiles, badges and the spinner.
//!
//! Every interactive control has hover, pressed, focus and disabled states
//! from the tokens, a role and a label for accessibility, short transitions
//! (and no motion at all when the user asked for reduced motion), and is
//! excluded from the hidden title bar's drag region.
//!
//! Builders are plain structs with chainable setters that take
//! `impl IntoReactive<T>` where the value may change; `IntoElement` renders
//! them.

use std::f32::consts::{PI, TAU};
use std::rc::Rc;

use nsh_core::canvas::LineCap;
use nsh_core::element::{
    AccessibilityRole, AccessibilityState, AdaptiveColor, BorderCurve, BoxShadow, PointerEvents,
};
use nsh_core::prelude::*;
use nsh_core::{AccessibilityValue, Checked, SvgPaint, TransformKind};

use crate::net::{DeviceType, Peer};
use crate::ui::icons::{IconName, icon};
use crate::ui::theme::{colors, control, focus_ring, motion, motion_ms, radius, space, text_size};

// --- shared pieces -------------------------------------------------------

/// A short shadow under a floating knob or indicator.
fn knob_shadow() -> [BoxShadow; 1] {
    [BoxShadow {
        offset_x: px(0.0),
        offset_y: px(1.0),
        blur: px(2.0),
        spread: px(0.0),
        color: nsh_core::color::rgba(0x1415182e).to_packed(),
        inset: false,
    }]
}

/// The reactive `disabled || loading` test, so a busy control blocks too.
fn blocked(disabled: Reactive<bool>, loading: Reactive<bool>) -> impl Fn() -> bool {
    move || disabled.get() || loading.get()
}

/// `Some(value)` as a static reactive, for the optional setters.
fn some<T: 'static>(value: T) -> Reactive<T> {
    Reactive::Static(value)
}

// --- button --------------------------------------------------------------

/// What a button is for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonKind {
    /// The main action: filled with the accent.
    #[default]
    Primary,
    /// A bordered, quiet action.
    Secondary,
    /// A text-only action.
    Ghost,
    /// An irreversible action: filled with danger.
    Danger,
}

/// A labelled button.
#[derive(Clone)]
pub struct Button {
    label: Reactive<SharedString>,
    kind: ButtonKind,
    icon: Option<IconName>,
    on_press: Option<Rc<dyn Fn()>>,
    disabled: Reactive<bool>,
    loading: Reactive<bool>,
    small: bool,
    full_width: bool,
    element_ref: Option<ElementRef>,
}

/// A button labelled `label`.
pub fn button(label: impl IntoReactive<SharedString>) -> Button {
    Button {
        label: label.into_reactive(),
        kind: ButtonKind::Primary,
        icon: None,
        on_press: None,
        disabled: Reactive::Static(false),
        loading: Reactive::Static(false),
        small: false,
        full_width: false,
        element_ref: None,
    }
}

impl Button {
    /// Its kind (default [`ButtonKind::Primary`]).
    pub fn kind(mut self, kind: ButtonKind) -> Self {
        self.kind = kind;
        self
    }

    /// A glyph before the label.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// What pressing it does.
    pub fn on_press(mut self, on_press: impl Fn() + 'static) -> Self {
        self.on_press = Some(Rc::new(on_press));
        self
    }

    /// Whether it is disabled.
    pub fn disabled(mut self, disabled: impl IntoReactive<bool>) -> Self {
        self.disabled = disabled.into_reactive();
        self
    }

    /// The small size: 26 pt instead of 32.
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    /// Fills its parent's width.
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    /// Whether it is running its action: shows a spinner and blocks presses.
    pub fn loading(mut self, loading: impl IntoReactive<bool>) -> Self {
        self.loading = loading.into_reactive();
        self
    }

    /// Binds the button to an [`ElementRef`], so the app can focus it (a
    /// sheet moves the keyboard focus to its first action when it opens).
    pub fn element_ref(mut self, element_ref: ElementRef) -> Self {
        self.element_ref = Some(element_ref);
        self
    }
}

impl IntoElement for Button {
    fn into_element(self) -> AnyElement {
        let height = if self.small {
            control::SMALL_HEIGHT
        } else {
            control::HEIGHT
        };
        let font = if self.small {
            text_size::SMALL
        } else {
            text_size::BODY
        };
        let text_color = match self.kind {
            ButtonKind::Primary => colors::ON_ACCENT,
            ButtonKind::Danger => colors::ON_DANGER,
            ButtonKind::Secondary | ButtonKind::Ghost => colors::TEXT,
        };
        let hover = match self.kind {
            ButtonKind::Primary => colors::ACCENT_HOVER,
            ButtonKind::Danger => colors::DANGER_HOVER,
            ButtonKind::Secondary | ButtonKind::Ghost => colors::HOVER,
        };
        let pressed = match self.kind {
            ButtonKind::Primary => colors::ACCENT_ACTIVE,
            ButtonKind::Danger => colors::DANGER,
            ButtonKind::Secondary | ButtonKind::Ghost => colors::TRACK,
        };
        let background = match self.kind {
            ButtonKind::Primary => colors::ACCENT,
            ButtonKind::Danger => colors::DANGER,
            ButtonKind::Secondary => colors::SURFACE,
            ButtonKind::Ghost => AdaptiveColor::TRANSPARENT,
        };
        let border = match self.kind {
            ButtonKind::Primary | ButtonKind::Danger | ButtonKind::Ghost => {
                AdaptiveColor::TRANSPARENT
            }
            ButtonKind::Secondary => colors::BORDER,
        };
        let loading = self.loading.clone();
        let is_blocked = blocked(self.disabled, self.loading);
        let press = self.on_press;
        let label = self.label;
        let glyph = self.icon;
        let anchor = self.element_ref;
        let tint = some(text_color);
        let glyph_view = {
            let loading = loading.clone();
            let tint = tint.clone();
            dynamic(move || {
                if loading.get() {
                    spinner()
                        .size(if self.small { 12.0 } else { 14.0 })
                        .into_element()
                } else if let Some(name) = glyph {
                    icon(name)
                        .size(if self.small { 14.0 } else { 16.0 })
                        .color(tint.clone())
                        .into_element()
                } else {
                    view().into_element()
                }
            })
        };
        view()
            .h(px(height))
            .when(self.full_width, |v| v.w_full())
            .when(!self.full_width, |v| v.flex_none())
            .when_some(anchor, |v, anchor| v.element_ref(&anchor))
            .px(px(if self.small { 10.0 } else { 14.0 }))
            .flex_row()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .rounded(px(radius::CONTROL))
            .border_curve(BorderCurve::Continuous)
            .border_1()
            .border_color(border)
            .bg(background)
            .text_color(text_color)
            .role(AccessibilityRole::Button)
            .label(label.clone())
            .focusable(true)
            .cursor_pointer()
            .window_drag(false)
            .disabled(is_blocked)
            .disabled_style(|s| s.opacity(0.42))
            .hover(move |s| s.bg(hover))
            .active(move |s| s.bg(pressed))
            .focus(focus_ring)
            .transition_all()
            .duration(motion_ms(motion::HOVER_MS))
            .ease_out()
            .on_press(move || {
                if let Some(press) = &press {
                    press();
                }
            })
            .child(glyph_view)
            .child(
                text(label)
                    .number_of_lines(1)
                    .text_size(px(font))
                    .font_weight(FontWeight::MEDIUM),
            )
            .into_element()
    }
}

// --- icon button ---------------------------------------------------------

/// A square, icon-only button (ghost style, 28 pt).
#[derive(Clone)]
pub struct IconButton {
    name: IconName,
    label: Reactive<SharedString>,
    on_press: Option<Rc<dyn Fn()>>,
    disabled: Reactive<bool>,
    tint: Reactive<AdaptiveColor>,
    size: f32,
}

/// An icon-only button showing `name`.
pub fn icon_button(name: IconName) -> IconButton {
    IconButton {
        name,
        label: Reactive::Static(SharedString::default()),
        on_press: None,
        disabled: Reactive::Static(false),
        tint: some(colors::MUTED),
        size: control::ICON,
    }
}

impl IconButton {
    /// Its accessible label (required: an icon is not text). Reactive like
    /// [`Button`]'s and [`Badge`]'s, so a translated label follows a language
    /// change without rebuilding the control.
    pub fn label(mut self, label: impl IntoReactive<SharedString>) -> Self {
        self.label = label.into_reactive();
        self
    }

    /// What pressing it does.
    pub fn on_press(mut self, on_press: impl Fn() + 'static) -> Self {
        self.on_press = Some(Rc::new(on_press));
        self
    }

    /// Whether it is disabled.
    pub fn disabled(mut self, disabled: impl IntoReactive<bool>) -> Self {
        self.disabled = disabled.into_reactive();
        self
    }

    /// The glyph's tint (default: muted).
    pub fn color(mut self, tint: impl IntoReactive<AdaptiveColor>) -> Self {
        self.tint = tint.into_reactive();
        self
    }

    /// Its width and height (default 28).
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
}

impl IntoElement for IconButton {
    fn into_element(self) -> AnyElement {
        let disabled = self.disabled.clone();
        let tint = self.tint.clone();
        let press = self.on_press;
        view()
            .size(px(self.size))
            .flex_shrink_0()
            .rounded(px(radius::CHIP))
            .items_center()
            .justify_center()
            .bg(AdaptiveColor::TRANSPARENT)
            .role(AccessibilityRole::Button)
            .label(self.label)
            .focusable(true)
            .cursor_pointer()
            .window_drag(false)
            .disabled(self.disabled)
            .disabled_style(|s| s.opacity(0.35))
            .hover(|s| s.bg(colors::HOVER))
            .active(|s| s.bg(colors::TRACK))
            .focus(focus_ring)
            .transition_colors()
            .duration(motion_ms(motion::HOVER_MS))
            .ease_out()
            .on_press(move || {
                if let Some(press) = &press {
                    press();
                }
            })
            .child(icon(self.name).size(self.size * 0.6).color(move || {
                // Keep the glyph quiet until the pointer arrives.
                if disabled.get() {
                    colors::FAINT
                } else {
                    tint.get()
                }
            }))
            .into_element()
    }
}

// --- toggle --------------------------------------------------------------

/// An on / off switch (38 × 22).
#[derive(Clone)]
pub struct Toggle {
    value: Reactive<bool>,
    on_change: Option<Rc<dyn Fn(bool)>>,
    label: Reactive<SharedString>,
    disabled: Reactive<bool>,
}

/// A switch for `value`.
pub fn toggle(value: impl IntoReactive<bool>) -> Toggle {
    Toggle {
        value: value.into_reactive(),
        on_change: None,
        label: Reactive::Static(SharedString::default()),
        disabled: Reactive::Static(false),
    }
}

impl Toggle {
    /// What flipping it does.
    pub fn on_change(mut self, on_change: impl Fn(bool) + 'static) -> Self {
        self.on_change = Some(Rc::new(on_change));
        self
    }

    /// Its accessible label, reactive like [`Button`]'s.
    pub fn label(mut self, label: impl IntoReactive<SharedString>) -> Self {
        self.label = label.into_reactive();
        self
    }

    /// Whether it is disabled.
    pub fn disabled(mut self, disabled: impl IntoReactive<bool>) -> Self {
        self.disabled = disabled.into_reactive();
        self
    }
}

impl IntoElement for Toggle {
    fn into_element(self) -> AnyElement {
        let value_state = self.value.clone();
        let value_border = self.value.clone();
        let value_fill = self.value.clone();
        let knob = self.value.clone();
        let value = self.value;
        let change = self.on_change;
        let label = self.label;
        view()
            .w(px(38.0))
            .h(px(22.0))
            .flex_shrink_0()
            .flex_row()
            .items_center()
            // Border-box: 1 pt border + 1 pt padding leaves an 18 pt
            // content height. The knob then has a symmetric 2 pt inset
            // in both states (38 - 2 * 2 - 18 = 16 pt travel).
            .p(px(1.0))
            .rounded(px(radius::PILL))
            .border_1()
            .border_color(move || {
                if value_border.get() {
                    colors::ACCENT
                } else {
                    colors::BORDER
                }
            })
            .bg(move || {
                if value_fill.get() {
                    colors::ACCENT
                } else {
                    colors::TRACK
                }
            })
            .role(AccessibilityRole::Switch)
            .label(label)
            .accessibility_state(move || AccessibilityState {
                checked: Some(if value_state.get() {
                    Checked::True
                } else {
                    Checked::False
                }),
                ..AccessibilityState::default()
            })
            .focusable(true)
            .cursor_pointer()
            .window_drag(false)
            .disabled(self.disabled)
            .disabled_style(|s| s.opacity(0.4))
            .hover(|s| s.opacity(0.92))
            .active(|s| s.opacity(0.82))
            .focus(focus_ring)
            .transition_all()
            .duration(motion_ms(motion::SELECT_MS))
            .ease_out()
            .on_press(move || {
                let next = !value.get();
                if let Some(change) = &change {
                    change(next);
                }
            })
            .child(
                view()
                    .size(px(18.0))
                    .flex_shrink_0()
                    .rounded(px(radius::PILL))
                    .bg(colors::KNOB)
                    .shadow(knob_shadow())
                    .translate_x(move || px(if knob.get() { 16.0 } else { 0.0 }))
                    .transition_transform()
                    .duration(motion_ms(motion::SELECT_MS))
                    .ease_out(),
            )
            .into_element()
    }
}

// --- segmented -----------------------------------------------------------

/// A segment's inset from the control's edge: a 1 pt border and 3 pt padding
/// on each side, which the indicator (inside the padding box) does not get.
const SEGMENT_INSET: f32 = 2.0 * 1.0 + 2.0 * 3.0;

/// Equal segments with an indicator that springs to the selected one.
#[derive(Clone)]
pub struct Segmented {
    options: Vec<SharedString>,
    selected: Reactive<usize>,
    on_change: Option<Rc<dyn Fn(usize)>>,
}

/// A segmented picker over `options`.
pub fn segmented(options: Vec<SharedString>) -> Segmented {
    Segmented {
        options,
        selected: Reactive::Static(0),
        on_change: None,
    }
}

impl Segmented {
    /// The selected segment's index.
    pub fn selected(mut self, selected: impl IntoReactive<usize>) -> Self {
        self.selected = selected.into_reactive();
        self
    }

    /// What choosing a segment does.
    pub fn on_change(mut self, on_change: impl Fn(usize) + 'static) -> Self {
        self.on_change = Some(Rc::new(on_change));
        self
    }
}

impl IntoElement for Segmented {
    fn into_element(self) -> AnyElement {
        let count = self.options.len().max(1);
        let width = signal(0.0_f32);
        // The indicator lives inside the border and the padding, so it is
        // narrower than the border-box width `on_layout` reports.
        let segment = computed(move || ((width.get() - SEGMENT_INSET).max(0.0)) / count as f32);
        let selected = self.selected;
        let x = animated_value(0.0);
        let placed = stored(false);
        let last = stored(f32::NAN);
        {
            let selected = selected.clone();
            effect(move || {
                let index = selected.get().min(count.saturating_sub(1));
                if width.get() <= 0.0 {
                    return;
                }
                let to = index as f32 * segment.get();
                // Every session revision (a transfer tick, a peer found) re-runs
                // the caller's `selected` closure; move the indicator only when
                // the target actually changed, not on every revision.
                if last.replace(to) == to {
                    return;
                }
                if reduce_motion() {
                    x.set(to);
                } else if placed.replace(true) {
                    x.spring(to, motion::SETTLE);
                } else {
                    x.set(to);
                }
            });
        }
        let change = self.on_change;
        let options: Vec<AnyElement> = self
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                let selected_state = selected.clone();
                let selected_color = selected.clone();
                let selected_weight = selected.clone();
                let change = change.clone();
                let label = option.clone();
                view()
                    .flex_1()
                    .h_full()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .px(px(space::SM))
                    .rounded(px(radius::CONTROL - 2.0))
                    .cursor_pointer()
                    .role(AccessibilityRole::Tab)
                    .label(option.clone())
                    .accessibility_state(move || AccessibilityState {
                        selected: selected_state.get() == index,
                        ..AccessibilityState::default()
                    })
                    .focusable(true)
                    .window_drag(false)
                    .hover(|s| s.opacity(0.9))
                    .active(|s| s.opacity(0.72))
                    .focus(focus_ring)
                    .transition_opacity()
                    .duration(motion_ms(motion::SELECT_MS))
                    .ease_out()
                    .on_press(move || {
                        if let Some(change) = &change {
                            change(index);
                        }
                    })
                    .child(
                        text(label)
                            .number_of_lines(1)
                            .text_size(px(text_size::BODY))
                            .text_color(move || {
                                if selected_color.get() == index {
                                    colors::TEXT
                                } else {
                                    colors::MUTED
                                }
                            })
                            .style_when(
                                move || selected_weight.get() == index,
                                |s| s.font_weight(FontWeight::SEMIBOLD),
                            )
                            .transition_colors()
                            .duration(motion_ms(motion::SELECT_MS))
                            .ease_out(),
                    )
                    .into_element()
            })
            .collect();
        view()
            .relative()
            .h(px(control::HEIGHT))
            .min_w(px(64.0 * count as f32 + 8.0))
            .flex_row()
            .p(px(3.0))
            .rounded(px(radius::CONTROL))
            .bg(colors::INSET)
            .border_1()
            .border_color(colors::BORDER)
            .role(AccessibilityRole::Tablist)
            .window_drag(false)
            .on_layout(move |frame: &Bounds<Points>| width.set(frame.size.width.0))
            .child(
                view()
                    .absolute()
                    .top(px(3.0))
                    .left(px(3.0))
                    .bottom(px(3.0))
                    .w(move || px(segment.get()))
                    .rounded(px(radius::CONTROL - 2.0))
                    .bg(colors::RAISED)
                    .border_1()
                    .border_color(colors::BORDER)
                    .shadow(knob_shadow())
                    .pointer_events(PointerEvents::None)
                    .bind_translate_x(&x),
            )
            .child(view().flex_1().flex_row().children(options))
            .into_element()
    }
}

// --- progress ------------------------------------------------------------

/// A 4 pt horizontal progress bar.
#[derive(Clone)]
pub struct ProgressBar {
    value: Reactive<f32>,
}

/// A progress bar for `value` (0..=1).
pub fn progress_bar(value: impl IntoReactive<f32>) -> ProgressBar {
    ProgressBar {
        value: value.into_reactive(),
    }
}

impl IntoElement for ProgressBar {
    fn into_element(self) -> AnyElement {
        let value = self.value;
        // The first value is a one-time read: doing it tracked would bind the
        // element that builds the bar to progress (and re-mount it per tick).
        let scale = animated_value(untracked(|| value.get()).clamp(0.0, 1.0));
        let target = value.clone();
        let last = stored((f32::NAN, false));
        effect(move || {
            let to = target.get().clamp(0.0, 1.0);
            let reduced = reduce_motion();
            // The caller's closure may subscribe to the whole session state
            // (`session.read`), so unrelated revisions reach this effect too:
            // retarget only when the value or the motion preference changed.
            if last.replace((to, reduced)) == (to, reduced) {
                return;
            }
            if reduced {
                scale.set(to);
            } else {
                scale.timing(to, motion::PROGRESS_MS, Easing::Linear);
            }
        });
        let announce = value.clone();
        view()
            .h(px(4.0))
            .w_full()
            .rounded(px(radius::PILL))
            .bg(colors::TRACK)
            .overflow_hidden()
            .role(AccessibilityRole::Progressbar)
            .accessibility_value(move || AccessibilityValue {
                min: Some(0.0),
                max: Some(1.0),
                now: Some(announce.get().clamp(0.0, 1.0)),
                text: None,
            })
            .child(
                view()
                    .h_full()
                    .w_full()
                    .rounded(px(radius::PILL))
                    .bg(colors::ACCENT)
                    .origin_left()
                    .bind_transform(TransformKind::ScaleX, &scale),
            )
            .into_element()
    }
}

/// A circular progress ring.
#[derive(Clone)]
pub struct ProgressRing {
    value: Reactive<f32>,
    size: f32,
    stroke: f32,
}

/// A progress ring for `value` (0..=1).
pub fn progress_ring(value: impl IntoReactive<f32>) -> ProgressRing {
    ProgressRing {
        value: value.into_reactive(),
        size: 32.0,
        stroke: 3.0,
    }
}

impl ProgressRing {
    /// Its diameter in points (default 32).
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Its stroke width in points (default 3).
    pub fn stroke(mut self, stroke: f32) -> Self {
        self.stroke = stroke;
        self
    }
}

impl IntoElement for ProgressRing {
    fn into_element(self) -> AnyElement {
        let size = self.size;
        let stroke = self.stroke;
        let radius = ((size - stroke) / 2.0).max(0.0);
        let center = size / 2.0;
        // One memo for both the drawing and the announced value: the canvas
        // redraws only when the fraction changes, not on every revision.
        let value = self.value;
        let shown = computed(move || value.get().clamp(0.0, 1.0));
        view()
            .size(px(size))
            .flex_shrink_0()
            .role(AccessibilityRole::Progressbar)
            .accessibility_value(move || AccessibilityValue {
                min: Some(0.0),
                max: Some(1.0),
                now: Some(shown.get()),
                text: None,
            })
            .child(
                canvas(move || {
                    let fraction = shown.get();
                    let painter = Painter::new()
                        .line_width(stroke)
                        .line_cap(LineCap::Round)
                        .stroke_color(color(colors::TRACK));
                    let painter = painter
                        .begin_path()
                        .arc(center, center, radius, 0.0, TAU)
                        .stroke();
                    if fraction <= 0.0 {
                        return painter;
                    }
                    painter
                        .stroke_color(color(colors::ACCENT))
                        .begin_path()
                        .arc(
                            center,
                            center,
                            radius,
                            -PI / 2.0,
                            -PI / 2.0 + TAU * fraction,
                        )
                        .stroke()
                })
                .size_full(),
            )
            .into_element()
    }
}

// --- device tile ---------------------------------------------------------

/// A rounded-square glyph for a peer's device type, with an online dot.
#[derive(Clone)]
pub struct DeviceTile {
    peer: Reactive<Peer>,
    size: f32,
}

/// The tile for `peer`.
pub fn device_tile(peer: impl IntoReactive<Peer>) -> DeviceTile {
    DeviceTile {
        peer: peer.into_reactive(),
        size: 38.0,
    }
}

impl DeviceTile {
    /// Its width and height (default 38).
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
}

impl IntoElement for DeviceTile {
    fn into_element(self) -> AnyElement {
        let size = self.size;
        let glyph = self.peer.clone();
        let dot = self.peer;
        let glyph_size = size * 0.5;
        view()
            .relative()
            .size(px(size))
            .flex_shrink_0()
            .rounded(px(radius::TILE))
            .border_curve(BorderCurve::Continuous)
            .border_1()
            .border_color(colors::BORDER)
            .bg(colors::RAISED)
            .items_center()
            .justify_center()
            .pointer_events(PointerEvents::None)
            .accessibility_elements_hidden(true)
            .child(dynamic(move || {
                let peer = glyph.get();
                icon(device_glyph(&peer))
                    .size(glyph_size)
                    .color(colors::MUTED)
                    .into_element()
            }))
            .child(
                view()
                    .absolute()
                    .right(px(2.0))
                    .bottom(px(2.0))
                    .size(px(8.0))
                    .rounded(px(radius::PILL))
                    .border_1()
                    .border_color(colors::RAISED)
                    .bg(move || {
                        if dot.get().address.is_some() {
                            colors::SUCCESS
                        } else {
                            colors::FAINT
                        }
                    }),
            )
            .into_element()
    }
}

/// The glyph a device type gets when it has no model name.
fn device_glyph(peer: &Peer) -> IconName {
    match peer.device_type {
        DeviceType::Desktop => IconName::Laptop,
        DeviceType::Mobile => IconName::Phone,
        DeviceType::Web => IconName::Globe,
        DeviceType::Headless => IconName::Tablet,
        DeviceType::Server => IconName::Globe,
        DeviceType::Unknown => IconName::Globe,
    }
}

// --- badge ---------------------------------------------------------------

/// What a badge means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BadgeTone {
    /// Quiet state: neutral.
    #[default]
    Neutral,
    /// The app's accent.
    Accent,
    /// Completed, online.
    Success,
    /// Failed, destructive.
    Danger,
    /// Needs attention.
    Warning,
}

/// A small capsule of text.
#[derive(Clone)]
pub struct Badge {
    label: Reactive<SharedString>,
    tone: BadgeTone,
}

/// A badge labelled `label`.
pub fn badge(label: impl IntoReactive<SharedString>) -> Badge {
    Badge {
        label: label.into_reactive(),
        tone: BadgeTone::Neutral,
    }
}

impl Badge {
    /// Its tone (default [`BadgeTone::Neutral`]).
    pub fn tone(mut self, tone: BadgeTone) -> Self {
        self.tone = tone;
        self
    }
}

impl IntoElement for Badge {
    fn into_element(self) -> AnyElement {
        let (background, text_color) = match self.tone {
            BadgeTone::Neutral => (colors::RAISED, colors::MUTED),
            BadgeTone::Accent => (colors::ACCENT_TINT, colors::ACCENT_FOREGROUND),
            BadgeTone::Success => (colors::SUCCESS_TINT, colors::SUCCESS),
            BadgeTone::Danger => (colors::DANGER_TINT, colors::DANGER),
            BadgeTone::Warning => (colors::WARNING_TINT, colors::WARNING),
        };
        view()
            .h(px(18.0))
            .flex_shrink_0()
            .px(px(7.0))
            .flex_row()
            .items_center()
            .rounded(px(radius::PILL))
            .bg(background)
            .text_color(text_color)
            .child(
                text(self.label)
                    .number_of_lines(1)
                    .text_size(px(text_size::CAPTION))
                    .font_weight(FontWeight::MEDIUM),
            )
            .into_element()
    }
}

// --- spinner -------------------------------------------------------------

/// A small spinning ring, drawn in the current text color.
#[derive(Clone)]
pub struct Spinner {
    size: f32,
}

/// A 14 pt spinner.
pub fn spinner() -> Spinner {
    Spinner { size: 14.0 }
}

impl Spinner {
    /// Its width and height (default 14).
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
}

impl IntoElement for Spinner {
    fn into_element(self) -> AnyElement {
        let size = self.size;
        let arc = move |painter: Painter| {
            let radius = ((size - 1.8) / 2.0).max(0.0);
            let center = size / 2.0;
            painter
                .begin_path()
                .arc(center, center, radius, -PI / 2.0, -PI / 2.0 + TAU * 0.72)
                .stroke()
        };
        view()
            .size(px(size))
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .accessibility_elements_hidden(true)
            // Reactive, so toggling the system setting mid-transfer stops or
            // resumes the spin instead of needing a re-mount.
            .animate_spin()
            .style_when(reduce_motion, |s| s.animate_none())
            .child(
                canvas(move || {
                    let painter = Painter::new()
                        .line_width(1.8)
                        .line_cap(LineCap::Round)
                        .stroke_style_from_view(SvgPaint::CurrentColor);
                    arc(painter)
                })
                .size_full(),
            )
            .into_element()
    }
}

// --- device row ----------------------------------------------------------

/// Where a [`PeerRow`] is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PeerRowKind {
    /// The shell sidebar's nearby list: 34 pt, the selected row tinted with an
    /// accent bar, the alias dimmed while the peer is unreachable.
    #[default]
    Sidebar,
    /// A device chooser popover row: 36 pt, an "online" subtitle and a check
    /// on the chosen device.
    Chooser,
}

/// A favorite toggle: its state, its accessible label and its action.
type Favorite = (Reactive<bool>, Reactive<SharedString>, Rc<dyn Fn()>);

/// A reveal button: its accessible label and its action.
type Reveal = (Reactive<SharedString>, Rc<dyn Fn()>);

/// A device row: tile, alias (and subtitle), the optional favorite toggle.
///
/// The one implementation of a peer row: the sidebar and the Send screen's
/// chooser both build it, so its focus ring, accessible label, selected state
/// and favorite affordance cannot drift between the two.
#[derive(Clone)]
pub struct PeerRow {
    peer: Reactive<Peer>,
    kind: PeerRowKind,
    subtitle: Reactive<SharedString>,
    selected: Reactive<bool>,
    focusable: Reactive<bool>,
    favorite: Option<Favorite>,
    press: Option<Rc<dyn Fn()>>,
}

/// A device row for `peer` in `kind`.
pub fn peer_row(peer: impl IntoReactive<Peer>, kind: PeerRowKind) -> PeerRow {
    PeerRow {
        peer: peer.into_reactive(),
        kind,
        subtitle: Reactive::Static(SharedString::default()),
        selected: Reactive::Static(false),
        focusable: Reactive::Static(true),
        favorite: None,
        press: None,
    }
}

impl PeerRow {
    /// Whether this is the chosen device.
    pub fn selected(mut self, selected: impl IntoReactive<bool>) -> Self {
        self.selected = selected.into_reactive();
        self
    }

    /// Whether the keyboard can reach it (a modal sheet's background rows
    /// step out of the Tab order).
    pub fn focusable(mut self, focusable: impl IntoReactive<bool>) -> Self {
        self.focusable = focusable.into_reactive();
        self
    }

    /// The quiet second line (a chooser row's "online").
    pub fn subtitle(mut self, subtitle: impl IntoReactive<SharedString>) -> Self {
        self.subtitle = subtitle.into_reactive();
        self
    }

    /// Adds the favorite toggle at the end of the row: its state, its
    /// accessible label and what pressing it does.
    pub fn favorite(
        mut self,
        favorite: impl IntoReactive<bool>,
        label: impl IntoReactive<SharedString>,
        on_toggle: impl Fn() + 'static,
    ) -> Self {
        self.favorite = Some((
            favorite.into_reactive(),
            label.into_reactive(),
            Rc::new(on_toggle),
        ));
        self
    }

    /// What choosing it does.
    pub fn on_press(mut self, on_press: impl Fn() + 'static) -> Self {
        self.press = Some(Rc::new(on_press));
        self
    }
}

impl IntoElement for PeerRow {
    fn into_element(self) -> AnyElement {
        let sidebar = self.kind == PeerRowKind::Sidebar;
        let label = self.peer.clone();
        let state = self.selected.clone();
        let check = self.selected.clone();
        let weight = self.selected.clone();
        let fill = self.selected.clone();
        let bar = self.selected.clone();
        let press = self.press;
        let focusable = self.focusable;
        let tile_sidebar = self.peer.clone();
        let alias_sidebar = self.peer.clone();
        let reachable = self.peer.clone();
        let tile_chooser = self.peer.clone();
        let alias_chooser = self.peer.clone();
        let subtitle = self.subtitle;
        let favorite = self.favorite;
        view()
            .relative()
            .h(px(if sidebar {
                control::DEVICE_ROW
            } else {
                control::DEVICE_POPOVER_ROW
            }))
            .flex_shrink_0()
            .flex_row()
            .items_center()
            .gap(px(space::SM))
            .when(sidebar, |row| {
                row.pl(px(space::SM + 2.0))
                    .pr(px(space::SM))
                    .rounded(px(radius::CHIP))
            })
            .when(!sidebar, |row| {
                row.px(px(space::SM)).rounded(px(radius::CONTROL))
            })
            .cursor_pointer()
            .role(AccessibilityRole::Button)
            .label(move || SharedString::from(label.get().alias))
            .accessibility_state(move || AccessibilityState {
                selected: state.get(),
                ..AccessibilityState::default()
            })
            .focusable(focusable)
            .window_drag(false)
            .when(sidebar, move |row| {
                row.style_when(fill, |style| style.bg(colors::SIDEBAR_ACCENT))
            })
            .hover(|style| style.bg(colors::HOVER))
            .active(move |style| {
                if sidebar {
                    style.bg(colors::TRACK)
                } else {
                    style.opacity(0.8)
                }
            })
            .focus(focus_ring)
            .transition_colors()
            .duration(motion_ms(motion::HOVER_MS))
            .ease_out()
            .when_some(press, |row, press| row.on_press(move || press()))
            .when(sidebar, move |row| {
                row.child(
                    view()
                        .absolute()
                        .left(px(1.0))
                        .top(px(9.0))
                        .w(px(2.5))
                        .h(px(16.0))
                        .rounded(px(radius::PILL))
                        .bg(colors::ACCENT)
                        .opacity(move || if bar.get() { 1.0_f32 } else { 0.0_f32 }),
                )
                .child(device_tile(move || tile_sidebar.get()).size(26.0))
                .child(
                    text(move || SharedString::from(alias_sidebar.get().alias))
                        .flex_1()
                        .number_of_lines(1)
                        .truncate()
                        .text_size(px(text_size::BODY))
                        .style_when(weight, |style| style.font_weight(FontWeight::SEMIBOLD))
                        .text_color(move || {
                            if reachable.get().address.is_some() {
                                colors::TEXT
                            } else {
                                colors::MUTED
                            }
                        }),
                )
            })
            .when(!sidebar, move |row| {
                row.child(device_tile(move || tile_chooser.get()).size(22.0))
                    .child(
                        view()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(
                                text(move || SharedString::from(alias_chooser.get().alias))
                                    .text_size(px(text_size::BODY))
                                    .truncate(),
                            )
                            .child(
                                text(move || subtitle.get())
                                    .text_size(px(text_size::CAPTION))
                                    .text_color(colors::MUTED),
                            ),
                    )
                    .child(when(
                        move || check.get(),
                        || icon(IconName::Check).size(16.0).color(colors::ACCENT),
                    ))
            })
            .children(favorite.map(|(is_favorite, label, toggle)| {
                icon_button(IconName::Pin)
                    .size(20.0)
                    .label(label)
                    .color(move || {
                        if is_favorite.get() {
                            colors::ACCENT
                        } else {
                            colors::FAINT
                        }
                    })
                    .on_press(move || toggle())
            }))
            .into_element()
    }
}

// --- file row ------------------------------------------------------------

/// A file line: glyph, name, right-aligned size and an optional reveal button.
///
/// Shared by the incoming request sheet and the transfer detail sheet so the
/// two file lists cannot drift apart.
#[derive(Clone)]
pub struct FileRow {
    glyph: IconName,
    name: Reactive<SharedString>,
    size: u64,
    inset: bool,
    reveal: Option<Reveal>,
}

/// A file row for a file of `size` bytes.
pub fn file_row(glyph: IconName, name: impl IntoReactive<SharedString>, size: u64) -> FileRow {
    FileRow {
        glyph,
        name: name.into_reactive(),
        size,
        inset: false,
        reveal: None,
    }
}

impl FileRow {
    /// Fills the row with the inset surface (rows divided by hairlines).
    pub fn inset(mut self) -> Self {
        self.inset = true;
        self
    }

    /// Adds the reveal-in-folder button with its accessible label.
    pub fn reveal(
        mut self,
        label: impl IntoReactive<SharedString>,
        on_press: impl Fn() + 'static,
    ) -> Self {
        self.reveal = Some((label.into_reactive(), Rc::new(on_press)));
        self
    }
}

impl IntoElement for FileRow {
    fn into_element(self) -> AnyElement {
        let size = self.size;
        let glyph = self.glyph;
        view()
            .flex_row()
            .items_center()
            .gap(px(space::SM))
            .h(px(control::FILE_ROW))
            .px(px(space::SM))
            .when(self.inset, |row| row.bg(colors::INSET))
            .child(icon(glyph).size(15.0).color(colors::MUTED))
            .child(
                text(self.name)
                    .text_size(px(text_size::SMALL))
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate(),
            )
            .child(
                text(crate::ui::format::bytes(size))
                    .text_size(px(text_size::CAPTION))
                    .text_color(colors::MUTED)
                    .tabular_nums(),
            )
            .children(self.reveal.map(|(label, on_press)| {
                icon_button(IconName::Folder)
                    .label(label)
                    .on_press(move || on_press())
            }))
            .into_element()
    }
}
