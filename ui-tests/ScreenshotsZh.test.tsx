// Chinese-UI captures for the README hero montage (docs/screenshots/overview.png).
//
// `ui-tests/Screenshots.test.tsx` pins `language: "en"` because its label-based
// interactions and assertions are written in English. The README is Chinese, so
// its montage is captured from this variant instead: the same screens, the same
// selectors (ids are language-independent), Chinese labels.
//
// The PNGs go to `screenshots/zh/` (the directory must exist; `app.screenshot`
// does not create it). See docs/BUILD.md for the composition step. The app
// entry is the repository root, one level above this file (`open("..")`).
//
//   mkdir -p screenshots/zh
//   LOCALSEND_DEMO=1        nsh test ui-tests/ScreenshotsZh.test.tsx --target macos
//   LOCALSEND_DEMO=incoming nsh test ui-tests/ScreenshotsZh.test.tsx --target macos --filter overlay
//   LOCALSEND_DEMO=pin      nsh test ui-tests/ScreenshotsZh.test.tsx --target macos --filter overlay
import { open, test } from "nsh/test";

// A partial settings JSON: serde fills the rest with defaults. Only the alias
// is pinned, so the montage does not show the machine's hostname; the receive
// folder stays the default `~/Downloads` of whoever runs the captures.
const SIZE = {
  width: 1200,
  height: 800,
  platform: "macos" as const,
  storage: { settings: '{"language":"zh","alias":"测试设备"}' },
};

/** Saves a PNG when the target draws; headless has no screenshots. */
async function shot(app: { screenshot(path: string): Promise<unknown> }, path: string) {
  try {
    await app.screenshot(path);
  } catch (error) {
    // Headless has no drawing target. On a real target a failed capture must
    // fail the test instead of leaving the montage sources stale.
    if (String(error).includes("needs a real platform")) return;
    throw error;
  }
}

type App = Awaited<ReturnType<typeof open>>;

/** Whether the demo devices are seeded. */
function demoReady(app: { shows(selector: string): boolean }): boolean {
  return app.shows("text:Peer MacBook Pro");
}

/**
 * Switches destination and waits for its screen, retrying a press that a
 * transient overlay/commit ate (the same retry `ui-tests/Screenshots.test.tsx` uses).
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

test("send and the transfer sheet", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app)) return;
  // Staged items, device chip and the bottom transfer card.
  await shot(app, "screenshots/zh/send.png");

  await app.press("id:transfer-card");
  await app.waitFor("text:传输详情", { timeout: 3000 });
  await shot(app, "screenshots/zh/transfer-sheet.png");
  await app.press("label:关闭");
  await app.waitFor("text:传输详情", { state: "gone", timeout: 3000 });
}, 120_000);

test("receive and history", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app)) return;
  await go(app, "接收", "receive");
  await app.waitFor("id:receive", { timeout: 3000 });
  await shot(app, "screenshots/zh/receive.png");

  await go(app, "历史", "history");
  await app.waitFor("id:history", { timeout: 3000 });
  await shot(app, "screenshots/zh/history.png");
}, 60_000);

test("settings", async () => {
  const app = await open("..", SIZE);
  if (!demoReady(app)) return;
  await app.press("label:设置");
  await app.waitFor("id:settings-sheet", { timeout: 3000 });
  await shot(app, "screenshots/zh/settings.png");
  await app.press("label:关闭");
}, 60_000);

test("dark mode", async () => {
  const app = await open("..", { ...SIZE, colorScheme: "dark" });
  if (!demoReady(app)) return;
  await shot(app, "screenshots/zh/send-dark.png");

  await go(app, "历史", "history");
  await app.waitFor("id:history", { timeout: 3000 });
  await shot(app, "screenshots/zh/history-dark.png");

  await go(app, "接收", "receive");
  await app.waitFor("id:receive", { timeout: 3000 });
  await shot(app, "screenshots/zh/receive-dark.png");
}, 120_000);

test("overlay (incoming request or PIN prompt)", async () => {
  const app = await open("..", SIZE);
  // The demo seed is applied during the first commits; wait for a seeded
  // device instead of sleeping.
  try {
    await app.waitFor("text:Peer MacBook Pro", { timeout: 3000 });
  } catch {
    // LOCALSEND_DEMO unset: nothing is seeded, the overlay cannot exist.
  }
  const texts = app.texts();
  if (texts.some((text) => text.includes("收到传输请求"))) {
    await shot(app, "screenshots/zh/incoming.png");
  } else if (texts.some((text) => text.includes("输入 PIN"))) {
    await shot(app, "screenshots/zh/pin.png");
  } else {
    await shot(app, "screenshots/zh/overlay-none.png");
  }
}, 60_000);
