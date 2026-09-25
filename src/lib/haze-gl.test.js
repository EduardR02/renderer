import { expect, test } from "bun:test";
import { shaderSources } from "./haze-gl.js";
import { TUNING } from "./haze.js";

/* The GPU's shaders carry no numbers of their own: every calibrated one is
   handed to them from haze.js (TUNING), which the CPU reads too, so the two
   renderers cannot drift apart. */

const sources = shaderSources();
const split = (source) => {
  const lines = source.split("\n");
  const defines = Object.fromEntries(
    lines.filter((l) => l.startsWith("#define ")).map((l) => {
      const [, name, value] = l.match(/^#define (\w+) (.*)$/);
      return [name, value];
    }),
  );
  const body = lines.filter((l) => !l.startsWith("#")).join("\n");
  return { defines, body };
};

test("every define is the calibrated value from haze.js", () => {
  for (const source of Object.values(sources)) {
    const { defines } = split(source);
    for (const [k, v] of Object.entries(TUNING.floats)) expect(Number(defines[k])).toBe(v);
    for (const [k, v] of Object.entries(TUNING.ints)) expect(Number(defines[k])).toBe(v);
    for (const [k, v] of Object.entries(TUNING.uints)) expect(Number(defines[k].replace(/u$/, ""))).toBe(v >>> 0);
    for (const [k, m] of Object.entries(TUNING.mat3)) {
      const cols = defines[k].match(/mat3\((.*)\)/)[1].split(",").map(Number);
      // By columns: column c is row-major m[c], m[3 + c], m[6 + c].
      for (let c = 0; c < 3; c++) for (let r = 0; r < 3; r++) expect(cols[c * 3 + r]).toBe(m[r * 3 + c]);
    }
  }
});

test("the shaders hold no numbers but the arithmetic's own", () => {
  // Pixel centres, smoothstep, cube roots, bytes, the hash's shifts, the
  // 2x2 of the half-size frost, and indices.
  const allowed = new Set([0, 1, 2, 3, 4, 15, 16, 0.5, 255]);
  for (const [name, source] of Object.entries(sources)) {
    const { body } = split(source);
    const numbers = [...body.matchAll(/(?<![\w.])(\d+\.?\d*(?:e[+-]?\d+)?)u?(?![\w.])/g)].map((m) => Number(m[1]));
    const stray = numbers.filter((n) => !allowed.has(n));
    if (stray.length) throw new Error(`${name}: ${[...new Set(stray)].join(", ")}`);
  }
});

test("every name the shaders use is defined, and every tuned number is used", () => {
  const names = new Set([...Object.keys(TUNING.floats), ...Object.keys(TUNING.ints), ...Object.keys(TUNING.uints), ...Object.keys(TUNING.mat3), "BLOCK"]);
  const used = new Set();
  for (const [name, source] of Object.entries(sources)) {
    const { body } = split(source);
    for (const [id] of body.matchAll(/\b[A-Z][A-Z0-9_]{2,}\b/g)) {
      if (!names.has(id)) throw new Error(`${name}: ${id} is not defined`);
      used.add(id);
    }
  }
  expect([...names].filter((n) => !used.has(n))).toEqual([]);
});
