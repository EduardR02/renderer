import { expect, test } from "bun:test";

// Like the other runes-store tests, exercise async identity without mounting UI.
globalThis.$state = (value) => value;
const requests = [];
globalThis.window = {
  __TAURI_INTERNALS__: {
    invoke(command, args) {
      return new Promise((resolve, reject) => requests.push({ command, args, resolve, reject }));
    },
  },
};
const { session, playback } = await import("./state.svelte.js");
const { personalApi, personalSaved } = await import("./personal.svelte.js");
const uri = "spotify:track:membership-track";
playback.current_uri = null;

function nextRequest(command) {
  const request = requests.shift();
  expect(request.command).toBe(command);
  return request;
}

test("a contains reply started before a confirmed like cannot undo that like", async () => {
  session.username = "membership-alice";
  const contains = personalApi.contains([uri]);
  const stale = nextRequest("personal_api_contains");
  const saving = personalApi.setSaved([uri], true);
  nextRequest("personal_api_set_saved").resolve(null);
  await saving;
  expect(personalSaved(uri)).toBe(true);
  stale.resolve([false]);
  await contains;
  expect(personalSaved(uri)).toBe(true);

  const removing = personalApi.setSaved([uri], false);
  nextRequest("personal_api_set_saved").resolve(null);
  await removing;
  expect(personalSaved(uri)).toBe(false);
});

test("membership responses and writes from another account stay out of the current account", async () => {
  session.username = "membership-bob";
  const contains = personalApi.contains([uri]);
  const stale = nextRequest("personal_api_contains");
  const saving = personalApi.setSaved([uri], true);
  const staleWrite = nextRequest("personal_api_set_saved");
  session.username = "membership-carol";
  stale.resolve([true]);
  staleWrite.resolve(null);
  await Promise.all([contains, saving]);
  expect(personalSaved(uri)).toBeUndefined();

  const own = personalApi.contains([uri]);
  nextRequest("personal_api_contains").resolve([false]);
  await own;
  expect(personalSaved(uri)).toBe(false);
});

test("a refused like leaves the confirmed unsaved membership intact", async () => {
  session.username = "membership-dave";
  const contains = personalApi.contains([uri]);
  nextRequest("personal_api_contains").resolve([false]);
  await contains;
  const saving = personalApi.setSaved([uri], true);
  nextRequest("personal_api_set_saved").reject("Spotify refused the request");
  await expect(saving).rejects.toBe("Spotify refused the request");
  expect(personalSaved(uri)).toBe(false);
});
