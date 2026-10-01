import { expect, test } from "bun:test";
import { mergeLikedRows, reconcileLikedRows, mergeLikedPage, applyLikedDelta } from "./liked-songs.js";

const row = (id, name = id) => ({ id, uri: `spotify:track:${id}`, name });

test("next pages retain loaded rows and reject duplicates or rows unliked during the request", () => {
  const first = row("one");
  const rows = [first, row("two")];
  const array = rows;
  mergeLikedRows(rows, [row("two"), row("removed"), row("three"), row("three"), {}], new Set(["spotify:track:removed"]));
  expect(rows).toBe(array);
  expect(rows[0]).toBe(first);
  expect(rows.map((track) => track.id)).toEqual(["one", "two", "three"]);
});

test("new likes precede the loaded collection without dropping later pages", () => {
  const rows = [row("one"), row("two"), row("later")];
  const later = rows[2];
  mergeLikedRows(rows, [row("new"), row("one"), row("new")], new Set(), true);
  expect(rows.map((track) => track.id)).toEqual(["new", "one", "two", "later"]);
  expect(rows[3]).toBe(later);
});

test("external reconciliation removes unlikes and refreshes fields without replacing retained row identity", () => {
  const retained = row("two", "Before");
  const rows = [row("removed"), retained, row("later")];
  const array = rows;
  reconcileLikedRows(rows, [row("new"), row("two", "After"), row("later"), row("two")]);
  expect(rows).toBe(array);
  expect(rows.map((track) => track.id)).toEqual(["new", "two", "later"]);
  expect(rows[1]).toBe(retained);
  expect(retained.name).toBe("After");
});

test("likes and unlikes preserve loaded pages and the opaque continuation used by the next page", () => {
  const collection = { tracks: [], nextCursor: null, loadedPages: 0, removed: new Set() };
  mergeLikedPage(collection, { tracks: [row("one"), row("two")], next_cursor: "opaque/page-two" });
  mergeLikedPage(collection, { tracks: [row("three"), row("four")], next_cursor: "opaque/page-three" });
  const rows = collection.tracks;
  const retained = rows[3];
  applyLikedDelta(collection, [row("two").uri], false);
  applyLikedDelta(collection, [row("new").uri], true, [row("new")]);
  expect(collection.tracks).toBe(rows);
  expect(collection.nextCursor).toBe("opaque/page-three");
  expect(collection.loadedPages).toBe(2);
  expect(rows.map((track) => track.id)).toEqual(["new", "one", "three", "four"]);
  expect(rows[3]).toBe(retained);
  mergeLikedPage(collection, { tracks: [row("two"), row("four"), row("five")], next_cursor: null });
  expect(rows.map((track) => track.id)).toEqual(["new", "one", "three", "four", "five"]);
  expect(collection.nextCursor).toBeNull();
  expect(collection.loadedPages).toBe(3);
});

test("re-liking a removed song restores it once and allows later cursor pages to omit its duplicate", () => {
  const collection = { tracks: [row("one")], nextCursor: "opaque/next", loadedPages: 1, removed: new Set() };
  applyLikedDelta(collection, [row("one").uri], false);
  applyLikedDelta(collection, [row("one").uri], true, [row("one")]);
  mergeLikedPage(collection, { tracks: [row("one"), row("two")], next_cursor: null });
  expect(collection.tracks.map((track) => track.id)).toEqual(["one", "two"]);
});
