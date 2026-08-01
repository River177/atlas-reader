import type { SelectionContextInput, TranslatedBlockView } from "@atlas/contracts";

export function captureTranslationSelection(
  root: HTMLElement,
  block: TranslatedBlockView,
): SelectionContextInput | undefined {
  const plainText = block.target?.plainText;
  const selection = window.getSelection();
  if (!plainText || !selection || selection.rangeCount !== 1 || selection.isCollapsed) {
    return undefined;
  }
  const range = selection.getRangeAt(0);
  if (!root.contains(range.startContainer) || !root.contains(range.endContainer)) {
    return undefined;
  }
  const mapped = mappedPlainTextRange(root, range);
  if (mapped) {
    const selectedText = plainText.slice(mapped.start, mapped.end);
    if (!selectedText.trim()) {
      return undefined;
    }
    return {
      blockId: block.blockId,
      sourceDigest: block.sourceDigest,
      startUtf16: mapped.start,
      endUtf16: mapped.end,
      selectedText,
    };
  }
  const selectedText = selection.toString();
  if (!selectedText.trim()) {
    return undefined;
  }
  const prefix = window.document.createRange();
  prefix.selectNodeContents(root);
  prefix.setEnd(range.startContainer, range.startOffset);
  const approximateStart = prefix.toString().length;
  const occurrences: number[] = [];
  for (
    let index = plainText.indexOf(selectedText);
    index >= 0;
    index = plainText.indexOf(selectedText, index + 1)
  ) {
    occurrences.push(index);
  }
  const startUtf16 = occurrences.sort(
    (left, right) => Math.abs(left - approximateStart) - Math.abs(right - approximateStart),
  )[0];
  if (startUtf16 === undefined) {
    return undefined;
  }
  const endUtf16 = startUtf16 + selectedText.length;
  if (plainText.slice(startUtf16, endUtf16) !== selectedText) {
    return undefined;
  }
  return {
    blockId: block.blockId,
    sourceDigest: block.sourceDigest,
    startUtf16,
    endUtf16,
    selectedText,
  };
}

function mappedPlainTextRange(
  root: HTMLElement,
  range: Range,
): { start: number; end: number } | undefined {
  const segments = Array.from(
    root.querySelectorAll<HTMLElement>("[data-plain-start][data-plain-end]"),
  ).filter((segment) => {
    try {
      return range.intersectsNode(segment);
    } catch {
      return false;
    }
  });
  if (segments.length === 0) {
    return undefined;
  }
  const firstSegment = segments[0];
  const lastSegment = segments[segments.length - 1];
  if (!firstSegment || !lastSegment) {
    return undefined;
  }
  const startSegment = containingSegment(root, range.startContainer) ?? firstSegment;
  const endSegment = containingSegment(root, range.endContainer) ?? lastSegment;
  const startBase = Number(startSegment.dataset.plainStart);
  const endBase = Number(endSegment.dataset.plainStart);
  const endLimit = Number(endSegment.dataset.plainEnd);
  if (![startBase, endBase, endLimit].every(Number.isInteger)) {
    return undefined;
  }
  const start =
    startBase + relativeTextOffset(startSegment, range.startContainer, range.startOffset, 0);
  const end =
    endBase +
    relativeTextOffset(endSegment, range.endContainer, range.endOffset, endLimit - endBase);
  return start < end ? { start, end } : undefined;
}

function containingSegment(root: HTMLElement, node: Node): HTMLElement | undefined {
  const element = node.nodeType === Node.ELEMENT_NODE ? (node as Element) : node.parentElement;
  const segment = element?.closest<HTMLElement>("[data-plain-start][data-plain-end]");
  return segment && root.contains(segment) ? segment : undefined;
}

function relativeTextOffset(
  segment: HTMLElement,
  container: Node,
  offset: number,
  fallback: number,
): number {
  if (!segment.contains(container) && segment !== container) {
    return fallback;
  }
  try {
    const prefix = window.document.createRange();
    prefix.selectNodeContents(segment);
    prefix.setEnd(container, offset);
    return prefix.toString().length;
  } catch {
    return fallback;
  }
}
