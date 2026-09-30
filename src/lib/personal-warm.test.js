import { beforeAll, expect, test } from "bun:test";

globalThis.$state ??= (value) => value;
let applyPlayback, libraryChanges, session, personal, personalApi, personalSaved;
beforeAll(async () => {
  ({ applyPlayback, libraryChanges, session } = await import("./state.svelte.js"));
  ({ personal, personalApi, personalSaved } = await import("./personal.svelte.js"));
});

function bridge(account) {
  const previous = globalThis.window;
  const requests = [];
  globalThis.window = { __TAURI_INTERNALS__: { invoke(command, args) {
    return new Promise((resolve, reject) => requests.push({ command, args, resolve, reject }));
  } } };
  applyPlayback({ ready: true, auth_state: "ready", username: account, playing: false, current_uri: null });
  personal.status = { connected: true, account_id: account };
  return { requests, restore() { globalThis.window = previous; } };
}
const uri = (id) => `spotify:track:${id}`;

test("visible membership readers share overlapping tracks and retain confirmed answers for menu opening", async () => {
  const { requests, restore } = bridge("warm-visible");
  try {
    const first = personalApi.warmMemberships([uri("a"), uri("b")]);
    const second = personalApi.warmMemberships([uri("b"), uri("c")]);
    await Promise.resolve();
    expect(requests[0].args.uris).toEqual([uri("a"), uri("b"), uri("c")]);
    const overlapping = personalApi.contains([uri("c"), uri("d")]);
    expect(requests[1].args.uris).toEqual([uri("d")]);
    requests[0].resolve([true, false, true]);
    requests[1].resolve([false]);
    await Promise.all([first, second]);
    expect(await overlapping).toEqual([true, false]);
    expect(personalSaved(uri("a"))).toBe(true);
    expect(personalSaved(uri("b"))).toBe(false);
    expect(await personalApi.contains([uri("b"), uri("a")])).toEqual([false, true]);
    expect(requests.length).toBe(2);
  } finally { restore(); }
});

test("new visible rows arriving during a warm read are not lost and batches respect Spotify's boundary", async () => {
  const { requests, restore } = bridge("warm-batch");
  try {
    const first = personalApi.warmMemberships([uri("first")]);
    await Promise.resolve();
    const laterUris = Array.from({ length: 41 }, (_, index) => uri(`later-${index}`));
    const later = personalApi.warmMemberships(laterUris);
    await Promise.resolve();
    expect(requests.map((request) => request.args.uris.length)).toEqual([1, 40, 1]);
    requests[0].resolve([true]);
    requests[1].resolve(Array(40).fill(false));
    requests[2].resolve([true]);
    await Promise.all([first, later]);
    expect(personalSaved(laterUris[0])).toBe(false);
    expect(personalSaved(laterUris[40])).toBe(true);
  } finally { restore(); }
});

test("external collection changes and logout/login to the same account invalidate membership authority", async () => {
  const { requests, restore } = bridge("warm-invalidation");
  try {
    const first = personalApi.contains([uri("a")]);
    requests[0].resolve([true]);
    await first;
    libraryChanges.savedTracks++;
    expect(personalSaved(uri("a"))).toBeUndefined();
    const current = personalApi.contains([uri("a")]);
    requests[1].resolve([false]);
    await current;
    expect(personalSaved(uri("a"))).toBe(false);
    const stale = personalApi.contains([uri("pending")]);
    applyPlayback({ auth_state: "logged_out", username: null, playing: false });
    applyPlayback({ ready: true, auth_state: "ready", username: "warm-invalidation", playing: false });
    requests[2].resolve([true]);
    await stale;
    expect(personalSaved(uri("a"))).toBeUndefined();
    expect(personalSaved(uri("pending"))).toBeUndefined();
    expect(session.username).toBe("warm-invalidation");
  } finally { restore(); }
});

test("a malformed membership reply cannot fabricate an unsaved answer and remains retryable", async () => {
  const { requests, restore } = bridge("warm-malformed");
  try {
    const first = personalApi.contains([uri("a")]);
    requests[0].resolve([]);
    await expect(first).rejects.toThrow("incomplete library membership");
    expect(personalSaved(uri("a"))).toBeUndefined();
    const retry = personalApi.contains([uri("a")]);
    requests[1].resolve([true]);
    expect(await retry).toEqual([true]);
    expect(personalSaved(uri("a"))).toBe(true);
  } finally { restore(); }
});

test("liking one URI during a batch cannot discard unrelated visible memberships or undo the like", async () => {
  const { requests, restore } = bridge("warm-write-race");
  try {
    const reading = personalApi.contains([uri("liked"), uri("unrelated")]);
    const saving = personalApi.setSaved([uri("liked")], true);
    requests[1].resolve(null);
    await saving;
    requests[0].resolve([false, true]);
    expect(await reading).toEqual([true, true]);
    expect(personalSaved(uri("liked"))).toBe(true);
    expect(personalSaved(uri("unrelated"))).toBe(true);
    expect(await personalApi.contains([uri("unrelated")])).toEqual([true]);
    expect(requests.length).toBe(2);
  } finally { restore(); }
});
