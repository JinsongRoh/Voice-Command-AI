import { render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { TranscriptEditor } from './TranscriptEditor';
import { mirrorParts } from './transcriptMirror';

/** jsdom has no layout: emulate a textarea whose content height follows its text. */
let scrollTops: WeakMap<Element, number>;
beforeEach(() => {
  scrollTops = new WeakMap();
  vi.spyOn(HTMLTextAreaElement.prototype, 'scrollHeight', 'get').mockImplementation(function (
    this: HTMLTextAreaElement,
  ) {
    return 20 + 21 * Math.max(1, this.value.split('\n').length);
  });
  vi.spyOn(HTMLTextAreaElement.prototype, 'clientHeight', 'get').mockReturnValue(100);
  vi.spyOn(Element.prototype, 'scrollTop', 'get').mockImplementation(function (this: Element) {
    return scrollTops.get(this) ?? 0;
  });
  vi.spyOn(Element.prototype, 'scrollTop', 'set').mockImplementation(function (
    this: Element,
    v: number,
  ) {
    scrollTops.set(this, v);
  });
});
afterEach(() => {
  vi.restoreAllMocks();
});

function editor(props: Partial<Parameters<typeof TranscriptEditor>[0]> = {}) {
  return (
    <TranscriptEditor
      id="t"
      value=""
      onChange={() => {}}
      corrections={[]}
      placeholder="Type"
      onSubmit={() => {}}
      {...props}
    />
  );
}

const lines = (n: number) => Array.from({ length: n }, (_, i) => `line ${i}`).join('\n');

describe('TranscriptEditor', () => {
  it('grows with its content', () => {
    const { rerender } = render(editor({ value: 'one line' }));
    const input = screen.getByRole('textbox');
    expect(input.style.height).toBe('41px');
    rerender(editor({ value: lines(6) }));
    expect(input.style.height).toBe(`${20 + 21 * 6}px`);
    rerender(editor({ value: '' }));
    expect(input.style.height).toBe('41px');
  });

  it('re-measures on window resize and when expanded', () => {
    const { rerender } = render(editor({ value: 'a' }));
    const input = screen.getByRole('textbox');
    input.style.height = '1px';
    window.dispatchEvent(new Event('resize'));
    expect(input.style.height).toBe('41px');
    rerender(editor({ value: 'a', expanded: true }));
    expect(input.parentElement?.className).toMatch(/expanded/);
  });

  it('follows live text unless the user scrolled up', () => {
    const { rerender } = render(editor({ value: lines(3), live: true }));
    const input = screen.getByRole('textbox') as HTMLTextAreaElement;
    rerender(editor({ value: lines(10), live: true }));
    expect(input.scrollTop).toBe(input.scrollHeight);
    // The user scrolls up: new text must not yank the view.
    input.scrollTop = 0;
    input.dispatchEvent(new Event('scroll'));
    rerender(editor({ value: lines(12), live: true }));
    expect(input.scrollTop).toBe(0);
    // Back at the bottom: following resumes.
    input.scrollTop = input.scrollHeight - 100;
    input.dispatchEvent(new Event('scroll'));
    rerender(editor({ value: lines(14), live: true }));
    expect(input.scrollTop).toBe(input.scrollHeight);
  });

  it('does not auto-scroll when not dictating', () => {
    const { rerender } = render(editor({ value: lines(3) }));
    const input = screen.getByRole('textbox');
    rerender(editor({ value: lines(10) }));
    expect(input.scrollTop).toBe(0);
  });

  it('dims the interim tail while live', () => {
    render(editor({ value: 'done so far', live: true, tailLength: 3 }));
    const tail = document.querySelector('[class*="tail"]');
    expect(tail).toHaveTextContent('far');
  });
});

describe('mirrorParts', () => {
  it('splits marks and the tail', () => {
    const c = [{ from: 'p n p m', to: 'pnpm', layer: 'base' as const }];
    expect(mirrorParts('run pnpm now', c)).toEqual([
      { text: 'run ', mark: false, tail: false },
      { text: 'pnpm', mark: true, tail: false },
      { text: ' now', mark: false, tail: false },
    ]);
    expect(mirrorParts('run pnpm now', c, 6)).toEqual([
      { text: 'run ', mark: false, tail: false },
      { text: 'pn', mark: true, tail: false },
      { text: 'pm', mark: true, tail: true },
      { text: ' now', mark: false, tail: true },
    ]);
    expect(mirrorParts('abc', [], 99)).toEqual([{ text: 'abc', mark: false, tail: true }]);
  });
});
