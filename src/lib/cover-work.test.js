import { expect, setSystemTime, test } from "bun:test";
import { boundedMisses, boundedReads } from "./cover-work.js";

const step = () => new Promise((resolve) => setTimeout(resolve, 0));

test("one key reads once and every caller gets that read's answer", async () => {
  let reads = 0;
  const read = boundedReads(
    async (value) => {
      reads += 1;
      return `${value}!`;
    },
    { max: 8, limit: 2 },
  );
  expect(await Promise.all([read("a", "a"), read("a", "a")])).toEqual(["a!", "a!"]);
  expect(reads).toBe(1);
  // A key that has already settled is answered from the memo.
  expect(await read("a", "a")).toBe("a!");
  expect(reads).toBe(1);
  expect(await read("b", "b")).toBe("b!");
  expect(reads).toBe(2);
});

test("reads run in the order asked for, never more than the limit at once", async () => {
  let active = 0;
  let peak = 0;
  const started = [];
  const read = boundedReads(
    async (value) => {
      active += 1;
      peak = Math.max(peak, active);
      started.push(value);
      await step();
      active -= 1;
      return value;
    },
    { max: 16, limit: 3 },
  );

  const asked = ["a", "b", "c", "d", "e", "f"];
  expect(await Promise.all(asked.map((value) => read(value, value)))).toEqual(asked);
  expect(peak).toBe(3);
  expect(started).toEqual(asked);
});

test("the memo keeps only its newest keys", async () => {
  let reads = 0;
  const read = boundedReads(
    async (value) => {
      reads += 1;
      return value;
    },
    { max: 2, limit: 1 },
  );

  await read("a", "a");
  await read("b", "b");
  await read("c", "c");
  expect(reads).toBe(3);
  // "a" fell out when "c" arrived, "c" did not.
  await read("a", "a");
  await read("c", "c");
  expect(reads).toBe(4);
});

test("a reader that throws still lets the queue drain", async () => {
  const read = boundedReads(
    (value) => {
      if (value === "bad") throw new Error("could not read");
      return Promise.resolve(value);
    },
    { max: 4, limit: 1 },
  );

  expect(read("bad", "bad")).rejects.toThrow("could not read");
  expect(await read("good", "good")).toBe("good");
});

test("one empty answer does not condemn a url, two in a row do", () => {
  const misses = boundedMisses(4);
  expect(misses.dead("a")).toBe(false);

  misses.miss("a");
  // A momentary failure must not hide the cover for the rest of the session:
  // the next tile to show this url still asks for it.
  expect(misses.dead("a")).toBe(false);
  expect(misses.dead("b")).toBe(false);

  misses.miss("a");
  // The same answer twice in a row is the url's, not the network's.
  expect(misses.dead("a")).toBe(true);
  expect(misses.dead("b")).toBe(false);
});

test("an answer that carried a url wipes the empty ones before it", () => {
  const misses = boundedMisses(4);
  misses.miss("a");
  misses.miss("a");
  expect(misses.dead("a")).toBe(true);

  misses.resolved("a");
  expect(misses.dead("a")).toBe(false);
  // ...so the next empty answer is measured on its own again.
  misses.miss("a");
  expect(misses.dead("a")).toBe(false);
});

test("the miss memory drops its oldest urls", () => {
  const misses = boundedMisses(2);
  misses.miss("a");
  misses.miss("a");
  misses.miss("b");
  misses.miss("b");
  expect(misses.dead("a")).toBe(true);

  misses.miss("c");
  misses.miss("c");
  expect(misses.dead("a")).toBe(false);
  expect(misses.dead("b")).toBe(true);
  expect(misses.dead("c")).toBe(true);
});

test("empty answers stop standing once they are old", () => {
  setSystemTime(new Date("2026-01-01T00:00:00Z"));
  try {
    const misses = boundedMisses(4);
    misses.miss("a");
    misses.miss("a");
    expect(misses.dead("a")).toBe(true);

    // An outage long enough to answer empty twice is still an outage: half a
    // minute later the url is asked for again instead of being hidden for the
    // rest of the session.
    setSystemTime(new Date("2026-01-01T00:00:31Z"));
    expect(misses.dead("a")).toBe(false);

    // A fresh pair of empty answers starts the silence over.
    misses.miss("a");
    misses.miss("a");
    expect(misses.dead("a")).toBe(true);
  } finally {
    setSystemTime();
  }
});
