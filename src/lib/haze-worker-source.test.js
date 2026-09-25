import { expect, test } from "bun:test";

test("a tainted source does not poison the worker's next readable picture", async () => {
  const previous = {
    canvas: globalThis.OffscreenCanvas,
    image: globalThis.ImageData,
    post: globalThis.postMessage,
    message: globalThis.onmessage,
  };
  const answers = [];

  class Canvas {
    constructor(w, h) {
      this._width = w;
      this._height = h;
      this.tainted = false;
      const canvas = this;
      this.ctx = {
        canvas,
        clearRect() {},
        drawImage(source) { canvas.tainted ||= source.tainted; },
        getImageData(x, y, w, h) {
          if (canvas.tainted) throw new DOMException("The canvas is tainted", "SecurityError");
          const data = new Uint8ClampedArray(w * h * 4);
          for (let i = 0; i < data.length; i += 4) data.set([160, 90, 70, 255], i);
          return { data };
        },
        putImageData() {},
      };
    }
    get width() { return this._width; }
    set width(value) { this._width = value; this.tainted = false; }
    get height() { return this._height; }
    set height(value) { this._height = value; this.tainted = false; }
    getContext() { return this.ctx; }
    transferToImageBitmap() { return { close() {} }; }
  }

  try {
    globalThis.OffscreenCanvas = Canvas;
    globalThis.ImageData = class { constructor(data, w, h) { Object.assign(this, { data, w, h }); } };
    globalThis.postMessage = (answer) => answers.push(answer);
    await import(`./haze.worker.js?taint-recovery=${Date.now()}`);
    globalThis.onmessage({ data: { type: "init", cpu: true } });
    const render = (key, tainted) => globalThis.onmessage({ data: {
      type: "render", key, seq: answers.length + 1, fade: "track",
      source: { width: 40, height: 40, tainted, close() {} },
      geo: { w: 16, h: 16, left: null, immersive: false },
      tint: [0.5, 0.3, 0.25], scale: 1,
    } });
    render("unreadable", true);
    expect(answers.at(-1).light).toEqual({ top: 1, bottom: 1 });
    render("readable", false);
    expect(answers.at(-1).light.top).toBeLessThan(1);
    expect(answers.at(-1).light.bottom).toBeLessThan(1);
  } finally {
    globalThis.OffscreenCanvas = previous.canvas;
    globalThis.ImageData = previous.image;
    globalThis.postMessage = previous.post;
    globalThis.onmessage = previous.message;
  }
});
