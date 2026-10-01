import { expect, test } from "bun:test";
import { filterEpisodes, orderEpisodes, indexEpisodeSearch } from "./episodes.js";

const episodes = [
  { track: { id: "old", name: "Episode 2" }, description: "A quiet morning", published_at: 100 },
  { track: { id: "unknown", name: "Episode 10" }, description: "Night sounds", published_at: null },
  { track: { id: "new", name: "Episode 3" }, description: "CITY walks", published_at: 300 },
  { track: { id: "same-date", name: "Episode 4" }, description: "City lights", published_at: 300 },
  { track: { id: "invalid", name: "Episode 11" }, published_at: NaN },
];
const ids = (rows) => rows.map((row) => row.track.id);
const searchIndex = indexEpisodeSearch(episodes);

test("publication order retains date ties, puts unknown dates last, and never reorders the browse result", () => {
  expect(ids(orderEpisodes(episodes))).toEqual(["new", "same-date", "old", "unknown", "invalid"]);
  expect(ids(orderEpisodes(episodes, "oldest"))).toEqual(["old", "new", "same-date", "unknown", "invalid"]);
  expect(ids(episodes)).toEqual(["old", "unknown", "new", "same-date", "invalid"]);
});

test("title order compares episode numbers naturally", () => {
  expect(ids(orderEpisodes(episodes, "title"))).toEqual(["old", "new", "same-date", "unknown", "invalid"]);
});

test("literal case-insensitive search covers titles and descriptions without changing the selected order", () => {
  const ordered = orderEpisodes(episodes, "oldest");
  expect(ids(filterEpisodes(ordered, "  city  ", searchIndex))).toEqual(["new", "same-date"]);
  expect(ids(filterEpisodes(ordered, "EPISODE 10", searchIndex))).toEqual(["unknown"]);
  expect(filterEpisodes(ordered, "   ", searchIndex)).toBe(ordered);
  expect(filterEpisodes(ordered, "[city]", searchIndex)).toEqual([]);
});

test("search finds episodes outside the rendered window and handles missing text", () => {
  const complete = Array.from({ length: 400 }, (_, index) => ({
    track: { id: String(index), name: `Episode ${index}` }, published_at: index,
    description: index === 399 ? "A hidden needle in the archive" : undefined,
  }));
  expect(ids(filterEpisodes(orderEpisodes(complete, "oldest"), "NEEDLE", indexEpisodeSearch(complete)))).toEqual(["399"]);
  const missing = [{ track: { id: "missing" } }];
  expect(filterEpisodes(missing, "needle", indexEpisodeSearch(missing))).toEqual([]);
  expect(filterEpisodes([], "needle", indexEpisodeSearch([]))).toEqual([]);
});

test("replacing browse data updates normalized search rather than retaining previous episode text", () => {
  const changed = [{ ...episodes[0], track: { id: "old", name: "A new title" }, description: "Changed description" }];
  const index = indexEpisodeSearch(changed);
  expect(ids(filterEpisodes(changed, "new title", index))).toEqual(["old"]);
  expect(filterEpisodes(changed, "quiet morning", index)).toEqual([]);
});
