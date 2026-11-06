// Headless UI tests for the LocalSend client: the root component, the
// navigation rail, the sheets and the empty states. `nsh test` runs these on
// the headless engine; the same tests also pass on a real platform with
// `nsh test ui-tests/App.test.tsx --target macos`. The app entry is the
// repository root, one level above this file (`open("..")`).
//
// Screenshots live in `ui-tests/Screenshots.test.tsx` (they need `--target macos`).
import { expect, open, test } from "nsh/test";

// The app follows the system locale on a fresh install; tests pin English so
// their text assertions do not depend on the machine running them.
const SIZE = {
  width: 1200,
  height: 800,
  platform: "macos" as const,
  storage: { settings: '{"language":"en"}' },
};

test("the shell renders", async () => {
  const app = await open("..", SIZE);
  expect(app.texts()).toContain("LocalSend");
  expect(app.texts()).toContain("Nearby devices");
  expect(app.texts()).toContain("Send");
  expect(app.texts()).toContain("Receive");
  expect(app.texts()).toContain("History");
  expect(app.texts()).toContain("Settings");
});

test("the destination rail switches screens", async () => {
  const app = await open("..", SIZE);
  await app.press("label:Receive");
  expect(app.texts()).toContain("Waiting for incoming transfers");
  await app.press("label:History");
  expect(app.texts()).toContain("No transfers yet");
  await app.press("label:Send");
  expect(app.texts()).toContain("Drop files to send");
});

test("the settings sheet opens from the rail and closes with Escape", async () => {
  const app = await open("..", SIZE);
  await app.press("label:Settings");
  expect(app.texts()).toContain("Device name");
  // Kept in sync by scripts/bump-version.sh; scripts/check.sh fails when this
  // string and the crate/nsh.json versions disagree, so a version bump cannot
  // leave this test red unnoticed.
  expect(app.texts()).toContain("Version 1.0.1");
  expect(app.texts()).toContain("Protocol v2.2");
  await app.keyboard.press("escape");
  expect(app.texts()).not.toContain("Device name");
});

test("the send screen offers files, folders and text", async () => {
  const app = await open("..", SIZE);
  expect(app.texts()).toContain("Drop files to send");
  expect(app.texts()).toContain("Choose files");
  expect(app.texts()).toContain("Choose folder");
  expect(app.texts()).toContain("Send text");
});

test("Escape closes settings while the alias field has focus", async () => {
  const app = await open("..", SIZE);
  await app.press("label:Settings");
  await app.waitFor("text:Device name");
  // Opening the sheet focuses the field; Escape must still dismiss it.
  await app.type("id:settings-alias", " Studio");
  await app.keyboard.press("escape");
  expect(app.texts()).not.toContain("Device name");
});

test("Escape dismisses the PIN sheet while its input has focus", async () => {
  const app = await open("..", SIZE);
  // Only seeded by `LOCALSEND_DEMO=pin`: the sheet has no other trigger.
  if (!app.shows("text:Enter PIN")) return;
  await app.type("id:pin-input", "1234");
  await app.keyboard.press("escape");
  await app.waitFor("text:Enter PIN", { state: "gone", timeout: 3000 });
});
