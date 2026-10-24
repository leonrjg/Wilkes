import type { ImagePage } from "@leonrjg/wilkes-reader";

/** Shared binary framing for HTTP and desktop IPC. Coordinates are MuPDF
 * points; the payload is one original image, never a complete document. */
export function decodeComicPage(packet: ArrayBuffer): ImagePage {
  if (packet.byteLength <= 16) throw new Error("Invalid comic page packet");
  const view = new DataView(packet);
  if (view.getUint32(0, false) !== 0x57435031) throw new Error("Invalid comic page packet");
  const width = view.getFloat32(4, true);
  const height = view.getFloat32(8, true);
  const kind = view.getUint32(12, true);
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0 || (kind !== 1 && kind !== 2)) {
    throw new Error("Invalid comic page dimensions or image type");
  }
  return { width, height, image: new Blob([new Uint8Array(packet, 16)], { type: kind === 1 ? "image/jpeg" : "image/png" }) };
}
