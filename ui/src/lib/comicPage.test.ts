import { describe, expect, it } from "vitest";
import { decodeComicPage } from "./comicPage";
function packet() {
  const data = new ArrayBuffer(20); const view = new DataView(data);
  view.setUint32(0, 0x57435031); view.setFloat32(4, 635.5, true);
  view.setFloat32(8, 891.25, true); view.setUint32(12, 1, true);
  return data;
}
describe("comic page transport", () => {
  it("keeps geometry and binary image payload", () => {
    const page = decodeComicPage(packet());
    expect(page.width).toBe(635.5); expect(page.height).toBe(891.25);
    expect(page.image.type).toBe("image/jpeg"); expect(page.image.size).toBe(4);
  });
  it("refuses truncated, incompatible, or invalid geometry packets", () => {
    expect(() => decodeComicPage(new ArrayBuffer(8))).toThrow();
    const data = packet(); const view = new DataView(data);
    view.setUint32(0, 0); expect(() => decodeComicPage(data)).toThrow();
    view.setUint32(0, 0x57435031); view.setFloat32(4, NaN, true);
    expect(() => decodeComicPage(data)).toThrow();
  });
});
