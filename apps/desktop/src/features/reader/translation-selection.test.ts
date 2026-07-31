import { afterEach, describe, expect, it } from "vitest";
import type { TranslatedBlockView } from "@atlas/contracts";

import { captureTranslationSelection } from "./translation-selection";

function translatedBlock(plainText: string): TranslatedBlockView {
  return {
    blockId: "block-1",
    sourceDigest: "source-digest",
    state: "ready",
    target: {
      plainText,
      atoms: [{ type: "text", value: plainText }],
    },
    safeMessage: null,
  };
}

function select(root: HTMLElement, start: number, end: number) {
  const node = root.firstChild;
  if (!node) {
    throw new Error("selection fixture has no text node");
  }
  const range = document.createRange();
  range.setStart(node, start);
  range.setEnd(node, end);
  const selection = window.getSelection();
  selection?.removeAllRanges();
  selection?.addRange(range);
}

afterEach(() => {
  window.getSelection()?.removeAllRanges();
  document.body.replaceChildren();
});

describe("captureTranslationSelection", () => {
  it("returns UTF-16 offsets for translated text containing a surrogate pair", () => {
    const root = document.createElement("div");
    root.textContent = "模型🙂采用该假设。";
    document.body.append(root);
    select(root, 4, 9);

    expect(captureTranslationSelection(root, translatedBlock(root.textContent))).toEqual({
      blockId: "block-1",
      sourceDigest: "source-digest",
      startUtf16: 4,
      endUtf16: 9,
      selectedText: "采用该假设",
    });
  });

  it("uses the DOM range to distinguish repeated translated text", () => {
    const root = document.createElement("div");
    root.textContent = "假设 A。假设 B。";
    document.body.append(root);
    select(root, 5, 7);

    expect(captureTranslationSelection(root, translatedBlock(root.textContent))).toMatchObject({
      startUtf16: 5,
      endUtf16: 7,
      selectedText: "假设",
    });
  });

  it("uses explicit plain-text offsets across structured table cells", () => {
    const root = document.createElement("div");
    root.innerHTML = [
      "<table><tbody><tr>",
      '<td><span data-plain-start="0" data-plain-end="1">甲</span></td>',
      '<td><span data-plain-start="4" data-plain-end="5">甲</span></td>',
      "</tr></tbody></table>",
    ].join("");
    document.body.append(root);
    const cells = root.querySelectorAll("span");
    if (!cells[0] || !cells[1]) {
      throw new Error("table selection fixtures are missing");
    }
    const range = document.createRange();
    range.setStart(cells[0].firstChild!, 0);
    range.setEnd(cells[1].firstChild!, 1);
    window.getSelection()?.removeAllRanges();
    window.getSelection()?.addRange(range);

    expect(captureTranslationSelection(root, translatedBlock("甲 | 甲"))).toEqual({
      blockId: "block-1",
      sourceDigest: "source-digest",
      startUtf16: 0,
      endUtf16: 5,
      selectedText: "甲 | 甲",
    });
  });

  it("rejects collapsed and cross-block selections", () => {
    const root = document.createElement("div");
    const other = document.createElement("div");
    root.textContent = "当前译文";
    other.textContent = "其他译文";
    document.body.append(root, other);
    select(root, 1, 1);
    expect(captureTranslationSelection(root, translatedBlock(root.textContent))).toBeUndefined();

    const range = document.createRange();
    range.setStart(root.firstChild!, 0);
    range.setEnd(other.firstChild!, 2);
    window.getSelection()?.removeAllRanges();
    window.getSelection()?.addRange(range);
    expect(captureTranslationSelection(root, translatedBlock(root.textContent))).toBeUndefined();
  });
});
