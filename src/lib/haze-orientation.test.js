import { expect, test } from "bun:test";
import { lightOf } from "./haze.js";

test("bottom-up WebGL pixels give the same top and bottom type light as top-down pixels", () => {
  for (const [w, h] of [[1, 1], [2, 2], [9, 17]]) {
    const topDown = new Uint8Array(w * h * 4);
    const bottomUp = new Uint8Array(topDown.length);
    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        const p = (y * w + x) * 4;
        topDown.set([(y * 31 + x * 7) % 256, (y * 13 + x * 43) % 256, (y * 71 + x * 11) % 256, 255], p);
        bottomUp.set(topDown.subarray(p, p + 4), ((h - 1 - y) * w + x) * 4);
      }
    }
    const shown = [w > 1 ? 1 : 0, 0, w, h];
    expect(lightOf({ data: bottomUp, w, h }, shown, true)).toEqual(lightOf({ data: topDown, w, h }, shown));
  }
});
