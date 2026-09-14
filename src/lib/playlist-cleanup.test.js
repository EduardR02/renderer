import { expect, test } from "bun:test";
import { cleanupChoices, cleanupDuration, cleanupPreview } from "./playlist-cleanup.js";

const tracks = [
  { uri: "spotify:track:a", name: "Live session", artist_ids: ["a"], artist_names: ["Artist A"], duration_ms: 180000 },
  { uri: "spotify:track:b", name: "Alive", artist_ids: ["b"], artist_names: ["Artist B"], duration_ms: 210000 },
  { uri: "spotify:track:a", name: "Alternate details", artist_ids: ["b"], artist_names: ["Artist B"], duration_ms: 240000 },
  { uri: "spotify:track:c", name: "Night drive", artist_ids: ["a"], artist_names: ["Artist A"], duration_ms: 300000 },
];

test("preview includes every copy the URI removal will delete, including nonmatching duplicates", () => {
  const artist = { field: "artist", choice: cleanupChoices(tracks, "artist").find((choice) => choice.id === "a") };
  const shorter = { field: "duration", operator: "shorter", duration: cleanupDuration("3:30") };
  const preview = cleanupPreview(tracks, [artist, shorter], "all");
  expect(preview.rows.map(({ index, extra }) => ({ index, extra }))).toEqual([
    { index: 0, extra: false }, { index: 2, extra: true },
  ]);
  expect(preview.uris).toEqual(["spotify:track:a"]);
  expect(preview.extraCount).toBe(1);
  expect(cleanupPreview(tracks, [artist, shorter], "any").rows.map(({ index }) => index)).toEqual([0, 2, 3]);
});

test("duration comparisons exclude exact boundaries and unknown lengths", () => {
  const rows = [...tracks, { uri: "spotify:track:unknown", duration_ms: 0 }];
  const rule = { field: "duration", operator: "longer", duration: cleanupDuration("3:30") };
  expect(cleanupPreview(rows, [rule], "all").rows.map(({ index }) => index)).toEqual([0, 2, 3]);
  // Row zero is included only because row two selects its URI.
  expect(cleanupPreview(rows, [{ ...rule, operator: "shorter" }], "all").rows.map(({ index }) => index)).toEqual([0, 2]);
});

test("whole-word filtering is Unicode-aware and never interprets user text as regex", () => {
  const rows = ["Live", "Alive", "LIVE session", "élive", "live_edition", "Live (2020)", "Other"].map((name, index) => ({ name, uri: `spotify:track:${index}` }));
  const rule = { field: "song", text: "live", operator: "words" };
  expect(cleanupPreview(rows, [rule], "all").rows.map(({ index }) => index)).toEqual([0, 2, 5]);
  expect(cleanupPreview(rows, [{ ...rule, operator: "contains" }], "all").rows.map(({ index }) => index)).toEqual([0, 1, 2, 3, 4, 5]);
  expect(cleanupPreview(rows, [{ ...rule, text: "(2020)", operator: "contains" }], "all").rows.map(({ index }) => index)).toEqual([5]);
  expect(cleanupPreview(rows, [{ ...rule, text: ".*" }], "all").rows).toEqual([]);
});
