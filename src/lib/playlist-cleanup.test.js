import { expect, test } from "bun:test";
import {
  cleanupChoices, cleanupDuration, filterCleanupChoices, cleanupPreview,
} from "./playlist-cleanup.js";

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

test("choices carry the folded name the filter matches on, ids included", () => {
  const rows = [
    { uri: "spotify:track:f", name: "Song", artist_names: ["ＡＣ／ＤＣ"], artist_ids: [""], duration_ms: 200000 },
    { uri: "spotify:track:g", name: "Other", artist_names: ["Beyoncé"], artist_ids: ["bey"], duration_ms: 200000 },
  ];
  const choices = cleanupChoices(rows, "artist");
  // Folded once at build time, because the filter runs per keystroke: the
  // fullwidth artist has to answer a query typed in plain ASCII. Folding is
  // NFKC plus case, not accent stripping, so "é" is still typed as "é".
  expect(choices.map(({ normalized }) => normalized).sort()).toEqual(["ac/dc", "beyoncé"]);
  expect(filterCleanupChoices(choices, "AC/DC").map(({ name }) => name)).toEqual(["ＡＣ／ＤＣ"]);
  expect(filterCleanupChoices(choices, "BEYONCÉ").map(({ key }) => key)).toEqual(["id:bey"]);
  expect(filterCleanupChoices(choices, "  ")).toHaveLength(2);
  expect(filterCleanupChoices(choices, "no such artist")).toEqual([]);
});

test("a namesake without an id matches on the folded name", () => {
  const rows = [
    { uri: "spotify:track:x", name: "One", artist_names: ["JAY-Z"], artist_ids: [""], duration_ms: 2000 },
    { uri: "spotify:track:y", name: "Two", artist_names: ["jay-z"], artist_ids: [""], duration_ms: 3000 },
    { uri: "spotify:track:z", name: "Three", artist_names: ["Jay Z"], artist_ids: [""], duration_ms: 4000 },
  ];
  const choice = cleanupChoices(rows, "artist").find((item) => item.normalized === "jay-z");
  // The two spellings of one artist are one choice, and the rule reaches both
  // rows by name because neither carries an id to be trusted instead.
  expect(choice.count).toBe(2);
  expect(cleanupPreview(rows, [{ field: "artist", choice }], "all").rows.map(({ index }) => index)).toEqual([0, 1]);
});
