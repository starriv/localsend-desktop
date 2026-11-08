# Design

The app should feel like Apple built LocalSend: quiet, precise, obviously
native on a Mac, and still legible on Windows and Linux. Nothing decorative
that does not carry meaning; motion only to explain a change.

## Product surface

One window, three destinations plus settings:

```
┌─────────────────────────┬──────────────────────────────────────────────┐
│  ● ● ●   LocalSend      │  (50 pt custom title bar, drag region)       │
│                         ├──────────────────────────────────────────────┤
│  Nearby devices         │                                              │
│   ○ MacBook Pro      ●  │   Send: drop zone / staged items             │
│   ○ iPhone 15 Pro    ●  │         device selector chip + Send button   │
│   ○ Living Room TV      │                                              │
│                         │                                              │
│  ──────────────         │                                              │
│  ⇧ Send                 │                                              │
│  ⇩ Receive              │                                              │
│  ↺ History              │                                              │
│                         │                                              │
│  ⚙ Settings    ⟨alias⟩  │                                              │
└─────────────────────────┴──────────────────────────────────────────────┘
```

- Sidebar 252 pt, content pane fills the rest. Window 1200 × 800.
- The window uses the hidden title bar: `set_title_bar(TitleBarStyle::Hidden,
  Some(px(50.0)))`. The sidebar's top 50 pt is the drag region, padded left by
  `title_bar().left` for the traffic lights.
- Incoming transfer → modal sheet centered over the content pane, dimmed
  scrim. Settings → sheet over the content pane (not a new window).
- Active transfer → bottom status card inside the content pane, above the
  content; clicking opens the transfer detail sheet.

### Menu bar

A menu bar (status) item is present the whole time the app runs on macOS.
Windows has a notification-area icon with localized Open LocalSend and Quit
actions; clicking the icon restores the window. With background mode enabled,
closing the window hides it and leaves transfers running. If the tray is
unavailable, close minimizes instead so the app remains reachable. The icon is
recreated after Explorer restarts. Linux has no tray.
The macOS item is the app mark drawn as a template
glyph; opening it shows, top to bottom:

- the nearby-device count (a summary row; it is not clickable);
- a status row: not connected to the local network, a pending incoming request
  (the row opens the window), or — while no transfer is running — ready to
  receive / ready but hidden from other devices;
- while transfers run, up to three rows of "To <name> — <percent>" /
  "From <name> — <percent>", each with a dimmed `transferred / total` bytes row
  under it; further transfers are summarized in one row;
- "Recent transfers": up to five finished transfers (a row opens History) with
  a dimmed day/time/size row under each, or an empty note;
- "Open LocalSend", then "Quit" (⌘Q; the quit confirmation still appears when a
  transfer is active).

Closing the window keeps the app running; while the window is closed (hidden)
the app takes AppKit's accessory activation policy, so the Dock icon disappears
and the menu bar item is how the window comes back. Showing it restores the
regular policy and the Dock icon. Row icons are the app's own glyphs rendered as
template `NSImage`s (the menu tints them), so the menu and the window share one
icon set.

## Visual language

| Property | Value |
|---|---|
| Accent | teal — light `#0c7d79`, dark `#35cfc6` (AA: white label 4.97:1) |
| Window background | `background` token (`#f4f4f6` / `#191a1d`) |
| Sidebar | `sidebar` token, 1 px `border` on the right |
| Cards | `card` fill, `radius-card` (14 pt), 1 px `border`, soft shadow in light mode only |
| Status tints | `success` `#17743f`, `warning` `#945800`, `danger` `#bd3037` (light; AA labels on their washes) |
| Controls | 32 pt tall (small: 26 pt), `radius-control` (9 pt) |
| Focus | 3 pt `ring` glow around the control |
| Selected device row | `sidebar-accent` fill, teal 2 pt left indicator, alias semibold |
| Type | system font; body 13, secondary 12 `muted-foreground`, captions 11 `faint`, titles 17/22 semibold |
| Spacing | 4 pt grid; screen gutters 28 pt; card padding 16–20 pt |

Colors come from `theme()` tokens (`theme.json`). Screens must not use raw hex.
The palette passed a WCAG AA contrast audit: text
pairs are ≥ 4.5:1 except 11 pt tertiary captions on the light page/sidebar
(3.9–4.2:1, icons there clear the 3:1 non-text bar).

## Component contract (`src/ui/`)

The design system implements exactly these names; screens are written against
them. Builders return view types with chainable setters (`impl IntoReactive<T>`
where a value may be reactive).

```rust
// ui/theme.rs
pub use nsh_base::theme;                  // theme().background, theme().card, …
pub mod colors { pub const ACCENT, TEXT, MUTED, FAINT, BORDER, SURFACE, HOVER,
                 TRACK, DANGER, SUCCESS, WARNING, SIDEBAR: AdaptiveColor; }
pub mod space { XS=4 SM=8 MD=12 LG=16 XL=24 XXL=32 }
pub mod radius { CONTROL=9.0 CARD=14.0 SHEET=18.0 PILL=999.0 }
pub mod text_size { CAPTION=11 SMALL=12 BODY=13 EMPHASIS=15 TITLE=17 DISPLAY=22 }
pub fn card_shadow() -> [BoxShadow; 1];
pub enum Appearance { System, Light, Dark }      // already exists

// ui/icons.rs
pub enum IconName { Send, Receive, History, Settings, Folder, File, Image,
  Text, Close, Check, ChevronRight, ChevronDown, ArrowUp, ArrowDown, Laptop,
  Phone, Tablet, Globe, Wifi, Lock, Plus, Trash, Copy, ExternalLink, Cancel,
  Refresh, Search, Pin, Users, Download, Upload, Info, Warning, Bolt, Link }
pub fn icon(name: IconName) -> Icon;             // 20×20 stroke glyph
impl Icon { fn size(f32) -> Self; fn color(impl IntoReactive<AdaptiveColor>) -> Self; }

// ui/controls.rs
pub enum ButtonKind { Primary, Secondary, Ghost, Danger }
pub fn button(label: impl IntoReactive<SharedString>) -> Button;
impl Button {
    fn kind(ButtonKind) -> Self;
    fn icon(IconName) -> Self;
    fn on_press(impl Fn() + 'static) -> Self;
    fn disabled(impl IntoReactive<bool>) -> Self;
    fn small() -> Self;              // 26 pt
    fn full_width() -> Self;
    fn loading(impl IntoReactive<bool>) -> Self;
}
pub fn icon_button(icon: IconName) -> IconButton;   // 28 pt square, Ghost style
impl IconButton { fn label(impl Into<SharedString>) /* a11y */; fn on_press(..); fn disabled(..); }

pub fn toggle(value: impl IntoReactive<bool>) -> Toggle;
impl Toggle { fn on_change(impl Fn(bool) + 'static) -> Self; fn label(..) -> Self; }

pub fn segmented(options: Vec<SharedString>) -> Segmented;
impl Segmented { fn selected(usize) -> Self; fn on_change(impl Fn(usize) + 'static) -> Self; }

pub fn progress_bar(value: impl IntoReactive<f32>) -> ProgressBar; // 4 pt, animated
pub fn progress_ring(value: impl IntoReactive<f32>) -> ProgressRing;
impl ProgressRing { fn size(f32) -> Self; fn stroke(f32) -> Self; }

pub fn device_tile(peer: impl IntoReactive<Peer>) -> DeviceTile;
impl DeviceTile { fn size(f32) -> Self; }      // rounded-square glyph by device type, online dot
pub fn badge(label: impl IntoReactive<SharedString>) -> Badge;
impl Badge { fn tone(BadgeTone) -> Self; }
pub enum BadgeTone { Neutral, Accent, Success, Danger, Warning }

pub fn spinner() -> Spinner;                    // 12–16 pt, reduce_motion aware

// ui/format.rs
pub fn bytes(size: u64) -> String;              // 1.2 GB
pub fn speed(bytes_per_second: f64) -> String;  // 24.7 MB/s
pub fn eta(remaining_seconds: f64) -> String;   // 00:42
pub fn clock(ms: u64) -> String;                // 14:03
pub fn day_label(ms: u64) -> DayLabel;          // Today / Yesterday / date
```

Every control must expose an accessibility role/label (`.role`,
`.label(..)`) and `disabled_style`; hover/active/focus states use the tokens.

Glyph drawing is backend-agnostic: `ui::icons` records every glyph through the
small `Pen` trait (a path builder in the same 20 × 20 box: `begin`, `move_to`,
`line_to`, `quad_to`, `close`, `arc`, `round_rect`, `ellipse`, then `stroke` or
`fill`). nsh's canvas implements `Pen` for the views, and
`platform::status_item` implements it over AppKit's `NSBezierPath` for the menu
bar images — one glyph definition, two backends, so the icon set cannot drift
between the window and the menu.

## Screens (`src/views/`)

Function contract (shell calls these; each takes `&Session` or a clone):

```rust
views::shell(session: Session, appearance: Appearance) -> impl IntoElement
views::send(&Session) -> impl IntoElement
views::receive(&Session) -> impl IntoElement
views::history(&Session) -> impl IntoElement
views::incoming_sheet(&Session) -> impl IntoElement   // overlay, empty when none
views::transfer_card(&Session) -> impl IntoElement    // active transfers, empty when none
views::settings_sheet(&Session) -> impl IntoElement   // overlay
views::pin_sheet(&Session) -> impl IntoElement        // overlay
views::toast(&Session) -> impl IntoElement            // bottom toast
```

### Send

- Empty state: dashed-border drop zone card filling the pane; 44 pt icon tile,
  `send.drop_title` 17 semibold, `send.drop_hint` 12 muted, two secondary
  buttons (Choose files / Choose folder) and a ghost "Send text".
- With items: a scrollable list of staged items (row 52 pt: file glyph or
  thumbnail, name 13, size 11 muted, remove icon button) and a footer bar:
  `send.items_count` + `send.total_size` on the left, a device selector chip
  (device tile + alias + chevron, opens a popover listing online devices) and
  the primary Send button on the right. Send is disabled unless a device and
  items are present, and while a send is active.
- Drag over the pane highlights the drop zone; drop adds files and folders.
- The whole pane is the drop target; text from the clipboard can be staged as
  a `ComposerItem::Text` (shown as a 3-line preview card).

### Receive

- Identity card: device tile, alias, `LocalSend / v2.2 / 192.168.x.x:53317`,
  discoverable toggle, PIN row (value or `receive.pin_unset`, with an edit
  action), save-folder row with a "Change" action and the path (truncated
  middle), quick-save toggle.
- Below: recent incoming transfers (active first), otherwise the quiet
  `receive.waiting` state with a subtle animated radar ring around the device
  tile (respect `reduce_motion`).
- Incoming request (modal sheet): peer tile + alias, total size and file count,
  the file list (max 6 rows then "+N more"), Decline (secondary) and Accept
  (primary). Accept → request disappears and the transfer card shows progress.

### History

- Grouped by day (`Today`, `Yesterday`, then `MMM d`), rows 56 pt: direction
  icon in a tinted circle, alias 13 semibold, file summary 11 muted, status
  badge, time 11 faint; row actions on hover: Open / Show in Finder / Copy text.
- Clear history in the header (destructive, with confirmation dialog).
- Empty state: `history.empty` + hint.

### Transfer card (bottom of the content pane)

- 64 pt card: device tile, "Sending to iPhone 15 Pro" / "Receiving from …",
  progress bar, `bytes(transferred) / bytes(total)`, speed, ETA, cancel icon
  button. Completed state shows a check and "Saved to …" with Open.
- Animate the progress bar; never jump more than once per frame.

### Settings sheet

- 560 pt wide sheet, title `settings.title`, sections with `section()` headers:
  General (device name text field, device type segmented, discoverable toggle),
  Receiving (save folder row, quick save toggle, PIN field), Appearance
  (light/dark/system segmented), Language (segmented), About (version, protocol,
  GitHub link). Apply immediately; no Save button. Escape/close dismisses.

## Motion

- Selection and hover: 120–160 ms ease-out.
- Sheet: 220 ms spring in, 160 ms out; scrim fades 160 ms.
- Progress: animated value, 250 ms linear; speed numbers update at most 4×/s.
- `reduce_motion()` disables springs (use fade only).

## Copy

Sentence case, no trailing punctuation in labels, verbs for actions
("Accept", "Decline", "Show in Finder"). All strings go through
`Session::text` / `i18n` keys; no literal user-facing text in views.
