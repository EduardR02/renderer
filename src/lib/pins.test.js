import { afterEach, expect, test } from "bun:test";

globalThis.$state = (value) => value;
globalThis.window = { __TAURI_INTERNALS__: { invoke: () => new Promise(() => {}) } };
const { session } = await import("./state.svelte.js");
const { pins, loadPins, isPinned, togglePin } = await import("./pins.svelte.js");
const originalStorage = globalThis.localStorage;
const originalUsername = session.username;
const stored = new Map();

function account(name) {
  session.username = name;
  globalThis.localStorage = {
    getItem: (key) => stored.get(key) ?? null,
    setItem: (key, value) => stored.set(key, value),
  };
  loadPins(name);
}

afterEach(() => {
  stored.clear();
  pins.account = null;
  pins.ids = [];
  session.username = originalUsername;
  globalThis.localStorage = originalStorage;
});

test("a new account defaults Liked Songs, but an explicit unpin survives restart", () => {
  account("new-user");
  expect(isPinned("liked")).toBe(true);
  togglePin("liked");
  expect(stored.get("sr.library-pins:new-user")).toBe("[]");
  pins.account = null;
  pins.ids = [];
  loadPins();
  expect(isPinned("liked")).toBe(false);
});

test("stored empty preferences survive account switches without leaking defaults", () => {
  stored.set("sr.library-pins:unpin-user", "[]");
  account("unpin-user");
  expect(isPinned("liked")).toBe(false);
  account("brand-new-user");
  expect(isPinned("liked")).toBe(true);
  account("unpin-user");
  expect(pins.ids).toEqual([]);
  account("");
  expect(isPinned("liked")).toBe(false);
});

test("stored playlist-only preferences never silently insert Liked Songs", () => {
  const id = "0123456789ABCDEFGHIJKL";
  stored.set("sr.library-pins:playlist-user", JSON.stringify([id, id, "bad-id", null]));
  account("playlist-user");
  expect(pins.ids).toEqual([id]);
  expect(isPinned("liked")).toBe(false);
});
