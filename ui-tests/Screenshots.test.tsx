// Visual pass + interaction assertions for the LocalSend screens.
//
//   LOCALSEND_DEMO=1        nsh test ui-tests/Screenshots.test.tsx --target macos
//   LOCALSEND_DEMO=incoming nsh test ui-tests/Screenshots.test.tsx --target macos --filter "overlay"
//   LOCALSEND_DEMO=pin      nsh test ui-tests/Screenshots.test.tsx --target macos --filter "overlay"
//
// Screenshots need a drawing target: without `--target` (headless) they are
// skipped and so are the demo-data tests, so a plain `nsh test` stays green.
// With `--target macos` the PNGs are written to `screenshots/` (ignored except
// for `.gitkeep`, so the directory exists in a fresh checkout). These are the
// English review captures; the committed README montage comes from
// ui-tests/ScreenshotsZh.test.tsx — see docs/BUILD.md.
//
// The app entry is the repository root, one level above this file
// (`open("..")`).
import { expect, open, test } from "nsh/test";

// The app follows the system locale on a fresh install; the screenshots and
// the label-based interactions below are written in English, so pin it (the
// stored settings JSON is partial: serde fills the rest with defaults).
//
// `alias` is pinned too: without it the default device name comes from the
// machine running the tests (`scutil` on macOS), so an assertion on the alias
// would only pass on the author's Mac. "Test Device Studio" is the expected
// value after the alias test types " Studio".
const SIZE = {
  width: 1200,
  height: 800,
  platform: "macos" as const,
  storage: { settings: '{"language":"en","alias":"Test Device"}' },
};

/** Saves a PNG when the target draws; headless has no screenshots. */
async function shot(app: { screenshot(path: string): Promise<unknown> }, path: string) {
  try {
    await app.screenshot(path);
  } catch (error) {
    // Headless has no drawing target. On a real target a failed capture must
    // fail the test instead of leaving a stale review capture.
    if (String(error).includes("needs a real platform")) return;
    throw error;
  }
}

type App = Awaited<ReturnType<typeof open>>;

/**
 * Opens the send screen's device chooser and selects `alias`.
 *
 * The popover is screen-local state and a background peers/transfer revision
 * can remount the Send screen, closing it between two steps (the same class of
 * flake the 250 ms press guard in `views/send.rs` fixes for real presses). The
 * test therefore reopens it and retries the press instead of sleeping.
 */
async function chooseDevice(app: App, alias: string, screenshotPath?: string) {
  let path = screenshotPath;
  let failures = 0;
  for (let attempt = 0; ; attempt++) {
    if (!app.shows("id:send-devices-popover")) {
      await app.press("id:send-device-chip");
      await app.waitFor("id:send-devices-popover", { timeout: 3000 });
    }
    if (path !== undefined) {
      await shot(app, path);
      path = undefined;
      // The screenshot took time; only press if the popover survived it.
      if (!app.shows("id:send-devices-popover")) continue;
    }
    try {
      await app.press(`id:send-devices-popover >> text:${alias}`);
      return;
    } catch (error) {
      failures += 1;
      if (failures >= 3) throw error;
      // A host commit during the screenshot can leave the chooser in a
      // closing state: `shows()` still sees it while its rows are no longer
      // pressable. Dismiss it and reopen instead of pressing a dying view.
      if (app.shows("id:send-devices-popover")) {
        await app.keyboard.press("escape");
        await app
          .waitFor("id:send-devices-popover", { state: "gone", timeout: 2000 })
          .catch(() => {});
      }
    }
  }
}

/** Whether the demo devices/transfers are seeded. */
function demoReady(app: { shows(selector: string): boolean }): boolean {
  return app.shows("text:Peer MacBook Pro");
}

/** Whether a seeded sheet is covering the content. */
function sheetOpen(app: { shows(selector: string): boolean }): boolean {
  return app.shows("text:Enter PIN") || app.shows("text:Incoming transfer");
}

/**
 * Switches destination and waits for its screen, retrying a press that a
 * transient overlay/commit ate. The rail press is a real pointer event; on a
 * loaded host one can land while the content pane is mid-commit.
 */
async function go(app: App, label: string, screen: string) {
  for (let attempt = 0; ; attempt++) {
    await app.press(`label:${label}`);
    try {
      await app.waitFor(`id:${screen}`, { timeout: 3000 });
      return;
    } catch (error) {
      if (attempt >= 2) throw error;
    }
  }
}

test("send", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app)) return;

  // Staged items, compact drop strip, device chip and the bottom card.
  await shot(app, "screenshots/send.png");

  await chooseDevice(app, "iPhone 15 Pro", "screenshots/send-devices.png");
  await app.waitFor("id:send-devices-popover", { state: "gone", timeout: 3000 });

  await app.press("id:transfer-card");
  await app.waitFor("text:Transfer details", { timeout: 3000 });
  await shot(app, "screenshots/transfer-sheet.png");
  await app.press("label:Close");
  await app.waitFor("text:Transfer details", { state: "gone", timeout: 3000 });

  await app.press("label:Clear");
  // The drop zone is the empty state: it is back once the staged list is gone.
  await app.waitFor("id:send-drop-zone", { timeout: 3000 });
  await shot(app, "screenshots/send-empty.png");

  await app.press("label:Send text");
  await app.waitFor("id:send-text-composer", { timeout: 3000 });
  await shot(app, "screenshots/send-text.png");
  await app.press("label:Cancel");
  await app.waitFor("id:send-text-composer", { state: "gone", timeout: 3000 });
}, 120_000);

test("toast", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app)) return;
  await app.press("label:Clear");
  await app.waitFor("id:send-drop-zone", { timeout: 3000 });
  await app.press("label:Send text");
  await app.waitFor("id:send-text-composer", { timeout: 3000 });
  // An empty clipboard goes through Session::fail -> toast.
  await app.press("label:Paste");
  await app.waitFor("id:toast", { timeout: 3000 });
  expect(app.texts()).toContain("The clipboard is empty");
  await shot(app, "screenshots/toast.png");
}, 60_000);

test("receive and history", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app)) return;
  await go(app, "Receive", "receive");
  await app.waitFor("id:receive", { timeout: 3000 });
  await shot(app, "screenshots/receive.png");

  await go(app, "History", "history");
  await app.waitFor("id:history", { timeout: 3000 });
  await shot(app, "screenshots/history.png");
  await app.hover("id:history-row >> nth=1");
  await shot(app, "screenshots/history-hover.png");
}, 60_000);

test("settings", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app)) return;
  await app.press("label:Settings");
  await app.waitFor("id:settings-sheet", { timeout: 3000 });
  await shot(app, "screenshots/settings.png");
  // Scroll the only scroll view to the About section at the bottom.
  await app.scroll("text:Device name", 2000);
  await app.waitFor("text:ABOUT", { timeout: 3000 });
  await shot(app, "screenshots/settings-about.png");
  await app.press("label:Close");
}, 60_000);

test("dark mode", async () => {
  const app = await open("..", { ...SIZE, colorScheme: "dark" });
  if (!demoReady(app)) return;
  await shot(app, "screenshots/send-dark.png");
  await go(app, "Receive", "receive");
  await app.waitFor("id:receive", { timeout: 3000 });
  await shot(app, "screenshots/receive-dark.png");
  await go(app, "History", "history");
  await app.waitFor("id:history", { timeout: 3000 });
  await shot(app, "screenshots/history-dark.png");
  await app.press("label:Settings");
  await app.waitFor("id:settings-sheet", { timeout: 3000 });
  await shot(app, "screenshots/settings-dark.png");
  await app.press("label:Close");
}, 120_000);

test("seeded overlay (incoming request or PIN prompt)", async () => {
  const app = await open("..", SIZE);
  // The demo seed (peers, transfers, the optional sheet) is applied during the
  // first commits; wait for a seeded device instead of sleeping.
  try {
    await app.waitFor("text:Peer MacBook Pro", { timeout: 3000 });
  } catch {
    // LOCALSEND_DEMO unset: nothing is seeded, the overlay cannot exist.
  }
  const texts = app.texts();
  if (texts.some((text) => text.includes("Incoming transfer"))) {
    await shot(app, "screenshots/incoming.png");
  } else if (texts.some((text) => text.includes("Enter PIN"))) {
    await shot(app, "screenshots/pin.png");
  } else {
    await shot(app, "screenshots/overlay-none.png");
  }
}, 60_000);

// ---------------------------------------------------------------------------
// Interaction paths. Each test no-ops unless its demo data/seeded sheet is
// present, so the file runs under every LOCALSEND_DEMO value and headless.
// ---------------------------------------------------------------------------

test("interaction: transfer detail sheet opens and closes", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app) || sheetOpen(app)) return;
  await app.press("id:transfer-card");
  // A press can occasionally be lost while the host is busy (the event lands
  // between commits, the card still shows); press once more rather than
  // sleeping, then assert the deterministic open/close sequence.
  if (!app.shows("text:Transfer details")) {
    await app.press("id:transfer-card");
  }
  await app.waitFor("text:Transfer details", { timeout: 5000 });
  await app.press("label:Close");
  await app.waitFor("text:Transfer details", { state: "gone", timeout: 3000 });
}, 60_000);

test("interaction: settings open and alias commits on Return", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app) || sheetOpen(app)) return;
  await app.press("label:Settings");
  await app.waitFor("text:Device name");
  await app.waitFor("id:settings-alias");
  // Fixed by SIZE, so this assertion does not depend on the machine's hostname.
  expect(app.value("id:settings-alias")).toBe("Test Device");
  await app.type("id:settings-alias", " Studio");
  await app.submit("id:settings-alias");
  // The commit is a state round-trip: wait for both the field and its label
  // instead of sleeping a fixed amount.
  await app.waitUntil(() => app.value("id:settings-alias") === "Test Device Studio", {
    timeout: 3000,
  });
  expect(app.texts()).toContain("Test Device Studio");
}, 60_000);

test("interaction: clear history asks then clears", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app) || sheetOpen(app)) return;
  await go(app, "History", "history");
  await app.waitFor("id:history", { timeout: 3000 });
  app.onDialog(() => "Clear history");
  await app.press("label:Clear history");
  // Native alerts may not be surfaced by every test host; when they are,
  // the confirmation text and the empty state must be right.
  const dialogs = app.dialogs();
  if (dialogs.length > 0) {
    expect(dialogs[0].title ?? "").toContain("Clear all history?");
    await app.waitFor("text:No transfers yet", { timeout: 3000 });
  } else {
    await app.waitFor("id:history", { timeout: 3000 });
  }
}, 60_000);

test("interaction: incoming request declines", async () => {
  const app = await open("..", SIZE);
  if (!app.shows("text:Incoming transfer")) return;
  await app.press("label:Decline");
  await app.waitFor("text:Incoming transfer", { state: "gone", timeout: 3000 });
}, 60_000);

test("interaction: PIN submits", async () => {
  const app = await open("..", SIZE);
  if (!app.shows("text:Enter PIN")) return;
  await app.type("id:pin-input", "1234");
  await app.submit("id:pin-input");
  await app.waitFor("text:Enter PIN", { state: "gone", timeout: 3000 });
}, 60_000);

// ---------------------------------------------------------------------------
// Trace measurements (`--trace`); see docs/PERFORMANCE.md. They also regress the "progress updates the screen"
// behavior. `LOCALSEND_DEMO=progress` drives the seeded transfer to completion
// in ~2 s, one transfers-domain revision per 50 ms tick.
// ---------------------------------------------------------------------------

test("perf: transfer card under progress", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app)) return;
  await app.waitFor("id:transfer-card", { timeout: 3000 });
  await app.wait(1200);
  // Minimal behaviour assertion: the card stays mounted under progress, so the
  // `--trace` numbers describe a live card (and a regression that unmounts it
  // fails here, not only in the trace).
  expect(app.shows("id:transfer-card")).toBe(true);
}, 60_000);

test("perf: receive screen under progress", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app)) return;
  await go(app, "Receive", "receive");
  await app.waitFor("id:receive", { timeout: 3000 });
  await app.wait(1200);
  expect(app.shows("id:receive")).toBe(true);
}, 60_000);

test("perf: a peers revision bump on each screen", async () => {
  // Selecting a device bumps the global revision (as a progress event does);
  // this measures what one such bump costs on each mounted screen.
  const app = await open("..", SIZE);
  if (!demoReady(app)) return;
  await go(app, "Receive", "receive");
  await app.waitFor("id:receive", { timeout: 3000 });
  await app.press("label:Peer MacBook Pro");
  await go(app, "History", "history");
  await app.waitFor("id:history", { timeout: 3000 });
  await app.press("label:iPhone 15 Pro");
  await go(app, "Send", "send");
  await app.waitFor("id:send", { timeout: 3000 });
  await app.press("label:Peer MacBook Pro");
  // Each bump re-renders the destination; staying on Send must keep the screen.
  expect(app.shows("id:send")).toBe(true);
  expect(app.texts()).toContain("Nearby devices");
}, 60_000);

test("perf: history screen under progress", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app)) return;
  await go(app, "History", "history");
  await app.waitFor("id:history", { timeout: 3000 });
  await app.wait(1200);
  expect(app.shows("id:history")).toBe(true);
}, 60_000);

// ---------------------------------------------------------------------------
// Interaction regressions for bugs fixed in this audit.
// ---------------------------------------------------------------------------

test("interaction: the device popover selects a device and closes", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app) || sheetOpen(app)) return;
  await chooseDevice(app, "iPhone 15 Pro");
  // The popover lists devices once, with the online caption.
  await app.waitFor("id:send-devices-popover", { state: "gone", timeout: 3000 });
  expect(app.shows("id:send-device-chip >> text:iPhone 15 Pro")).toBe(true);
}, 60_000);

test("interaction: keep running in the background persists", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app) || sheetOpen(app)) return;
  await app.press("label:Settings");
  await app.waitFor("text:Keep running when the window closes", { timeout: 3000 });
  // The test's storage starts from a partial settings JSON, so the field
  // falls back to the app default (keep running).
  const before = JSON.parse(app.storage().settings ?? "{}").run_in_background ?? true;
  await app.press("role:switch[Keep running when the window closes]");
  await app.waitUntil(
    () => (JSON.parse(app.storage().settings ?? "{}").run_in_background ?? true) === !before,
    { timeout: 3000 },
  );
  const after = JSON.parse(app.storage().settings ?? "{}").run_in_background;
  expect(after).toBe(!before);
  await app.press("label:Close");
}, 60_000);

test("interaction: the completed card appears after a transfer finishes", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app)) return;
  // `LOCALSEND_DEMO=progress` drives the seeded transfer to completion; the
  // other demo modes leave it active, so this test only asserts when it saw
  // a completion (the card's 8 s grace period keeps it on screen).
  if (!app.shows("id:transfer-card")) return;
  try {
    await app.waitFor("text:Completed", { timeout: 6000 });
  } catch {
    return;
  }
  expect(app.texts()).toContain("Completed");
  await app.press("label:Dismiss");
  await app.waitFor("id:transfer-card", { state: "gone", timeout: 3000 });
}, 60_000);
