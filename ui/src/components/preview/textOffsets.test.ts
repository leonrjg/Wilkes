import { describe, expect, it } from "vitest";
import {
  utf16OffsetToUtf8ByteOffset,
  utf8ByteOffsetToUtf16Offset,
  utf8ByteRangeToUtf16Range,
  textSelectionFromUtf16Range,
} from "./textOffsets";

describe("text offset conversion", () => {
  const text = "aé🙂z";

  it("converts UTF-16 offsets to UTF-8 byte offsets", () => {
    expect(utf16OffsetToUtf8ByteOffset(text, 0)).toBe(0);
    expect(utf16OffsetToUtf8ByteOffset(text, 1)).toBe(1);
    expect(utf16OffsetToUtf8ByteOffset(text, 2)).toBe(3);
    expect(utf16OffsetToUtf8ByteOffset(text, 4)).toBe(7);
    expect(utf16OffsetToUtf8ByteOffset(text, 5)).toBe(8);
  });

  it("converts UTF-8 byte offsets to UTF-16 offsets without splitting characters", () => {
    expect(utf8ByteOffsetToUtf16Offset(text, 0)).toBe(0);
    expect(utf8ByteOffsetToUtf16Offset(text, 3)).toBe(2);
    expect(utf8ByteOffsetToUtf16Offset(text, 7)).toBe(4);
    expect(utf8ByteRangeToUtf16Range(text, { start: 3, end: 7 })).toEqual({
      start: 2,
      end: 4,
    });
  });

  it("builds a text bookmark target using UTF-8 bytes and line-local columns", () => {
    expect(textSelectionFromUtf16Range("first\né🙂 last", 6, 9, 2, 6)).toEqual({
      quote: "é🙂",
      origin: { TextFile: { line: 2, col: 0 } },
      text_range: { start: 6, end: 12 },
      rects: [],
    });
  });
});
