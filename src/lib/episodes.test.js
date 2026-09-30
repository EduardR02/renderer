import { expect, test } from "bun:test";
import { filterEpisodes, orderEpisodes } from "./episodes.js";

const episodes = [
  { id: "old", name: "Episode 2", description: "A quiet morning", published_at: 100 },
  { id: "unknown", name: "Episode 10", description: "Night sounds", published_at: null },
  { id: "new", name: "Episode 3", description: "CITY walks", published_at: 300 },
  { id: "same-date", name: "Episode 4", description: "City lights", published_at: 300 },
  { id: "invalid", name: "Episode 11", published_at: NaN },
];
const ids = (rows) => rows.map((row) => row.id);

test("publication order defaults newest first, retains date ties, and never reorders the browse result", () => {
  expect(ids(orderEpisodes(episodes))).toEqual(["new", "same-date", "old", "unknown", "invalid"]);
  expect(ids(orderEpisodes(episodes, "oldest"))).toEqual(["old", "new", "same-date", "unknown", "invalid"]);
  expect(ids(episodes)).toEqual(["old", "unknown", "new", "same-date", "invalid"]);
});

test("title order compares episode numbers naturally", () => {
  expect(ids(orderEpisodes(episodes, "title"))).toEqual(["old", "new", "same-date", "unknown", "invalid"]);
});

test("literal case-insensitive search covers titles and descriptions without changing the selected order", () => {
  const ordered = orderEpisodes(episodes, "oldest");
  expect(ids(filterEpisodes(ordered, "  city  "))).toEqual(["new", "same-date"]);
  expect(ids(filterEpisodes(ordered, "EPISODE 10"))).toEqual(["unknown"]);
  expect(filterEpisodes(ordered, "   ")).toBe(ordered);
  expect(filterEpisodes(ordered, "[city]")).toEqual([]);
});

test("search finds episodes outside the rendered window and handles empty or missing text", () => {
  const complete = Array.from({ length: 400 }, (_, index) => ({
    id: String(index), name: `Episode ${index}`, published_at: index,
    description: index === 399 ? "A hidden needle in the archive" : undefined,
  }));
  expect(ids(filterEpisodes(orderEpisodes(complete, "oldest"), "NEEDLE"))).toEqual(["399"]);
  expect(filterEpisodes([{ id: "missing" }], "needle")).toEqual([]);
  expect(filterEpisodes(orderEpisodes([]), "needle")).toEqual([]);
});
