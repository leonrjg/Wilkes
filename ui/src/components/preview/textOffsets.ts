import type { ByteRange } from "./documentCoordinates";
import type { DocumentSelection } from "./selection";

const encoder = new TextEncoder();

/** Convert a CodeMirror/JavaScript UTF-16 offset to a persisted UTF-8 byte offset. */
export function utf16OffsetToUtf8ByteOffset(text: string, offset: number): number {
  const clamped = Math.max(0, Math.min(offset, text.length));
  return encoder.encode(text.slice(0, clamped)).length;
}

/** Convert a persisted UTF-8 byte offset to a CodeMirror/JavaScript UTF-16 offset. */
export function utf8ByteOffsetToUtf16Offset(text: string, offset: number): number {
  const target = Math.max(0, offset);
  let bytes = 0;
  let utf16 = 0;

  for (const character of text) {
    const characterBytes = encoder.encode(character).length;
    if (bytes + characterBytes > target) break;
    bytes += characterBytes;
    utf16 += character.length;
  }

  return utf16;
}

export function utf8ByteRangeToUtf16Range(text: string, range: ByteRange): ByteRange {
  return {
    start: utf8ByteOffsetToUtf16Offset(text, range.start),
    end: utf8ByteOffsetToUtf16Offset(text, range.end),
  };
}

export function textSelectionFromUtf16Range(
  text: string,
  from: number,
  to: number,
  line: number,
  lineStart: number,
): DocumentSelection {
  return {
    quote: text.slice(from, to).trim(),
    origin: {
      TextFile: {
        line,
        col: utf16OffsetToUtf8ByteOffset(text.slice(lineStart, from), from - lineStart),
      },
    },
    text_range: {
      start: utf16OffsetToUtf8ByteOffset(text, from),
      end: utf16OffsetToUtf8ByteOffset(text, to),
    },
    rects: [],
  };
}
