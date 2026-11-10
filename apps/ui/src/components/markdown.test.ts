import { describe, expect, it } from 'vitest';

import { hasMarkdown, markdownRuns, parseInline, parseMarkdown, safeHref } from './markdown';

describe('markdown parser', () => {
  it('parses GFM tables with alignment', () => {
    const [table] = parseMarkdown(
      '| Check | Result |\n| :--- | ---: |\n| server | ok, `30` times |\n| app | ok |',
    );
    expect(table).toMatchObject({ type: 'table', align: ['left', 'right'] });
    if (table?.type !== 'table') throw new Error('no table');
    expect(table.rows).toHaveLength(2);
    expect(table.rows[0]![1]).toEqual([
      { type: 'text', text: 'ok, ' },
      { type: 'code', text: '30' },
      { type: 'text', text: ' times' },
    ]);
  });

  it('parses headings, fences, quotes, rules and nested lists', () => {
    const blocks = parseMarkdown(
      [
        '## Plan',
        '',
        '1. build',
        '2. test',
        '   - unit',
        '   - e2e',
        '',
        '```bash',
        'pnpm test',
        '```',
        '> note',
        '',
        '---',
        'done',
      ].join('\n'),
    );
    expect(blocks.map((b) => b.type)).toEqual(['h', 'list', 'code', 'quote', 'hr', 'p']);
    const list = blocks[1];
    if (list?.type !== 'list') throw new Error('no list');
    expect(list.ordered).toBe(true);
    expect(list.items[1]!.map((b) => b.type)).toEqual(['p', 'list']);
    expect(blocks[2]).toEqual({ type: 'code', lang: 'bash', code: 'pnpm test' });
  });

  it('parses emphasis, links and autolinks; unsafe links stay text', () => {
    expect(parseInline('**bold** and *it* and ~~old~~')).toEqual([
      { type: 'strong', children: [{ type: 'text', text: 'bold' }] },
      { type: 'text', text: ' and ' },
      { type: 'em', children: [{ type: 'text', text: 'it' }] },
      { type: 'text', text: ' and ' },
      { type: 'del', children: [{ type: 'text', text: 'old' }] },
    ]);
    expect(parseInline('[docs](https://vca.metavu.io/docs) or https://x.dev/a.')).toEqual([
      {
        type: 'link',
        href: 'https://vca.metavu.io/docs',
        children: [{ type: 'text', text: 'docs' }],
      },
      { type: 'text', text: ' or ' },
      {
        type: 'link',
        href: 'https://x.dev/a',
        children: [{ type: 'text', text: 'https://x.dev/a' }],
      },
      { type: 'text', text: '.' },
    ]);
    expect(parseInline('[x](javascript:alert(1))')).toEqual([{ type: 'text', text: 'x' }]);
    expect(safeHref('file:///etc/passwd')).toBeNull();
    // snake_case words are not emphasis.
    expect(parseInline('a_b_c')).toEqual([{ type: 'text', text: 'a_b_c' }]);
  });

  it('keeps raw HTML as text', () => {
    expect(parseInline('<img src=x onerror=alert(1)>')).toEqual([
      { type: 'text', text: '<img src=x onerror=alert(1)>' },
    ]);
  });

  it('lists the visible text runs for search', () => {
    expect(markdownRuns('| a | b |\n|---|---|\n| **c** | d |')).toEqual(['a', 'b', 'c', 'd']);
    expect(hasMarkdown('just words\non two lines')).toBe(false);
    expect(hasMarkdown('- item')).toBe(true);
  });
});

describe('<br> in table cells', () => {
  it('turns <br> into a line break and never shows the tag', () => {
    const [t] = parseMarkdown('| a | b |\n|---|---|\n| x | • one<br>• two<BR/>• three |');
    expect(t?.type).toBe('table');
    if (t?.type !== 'table') return;
    const cell = t.rows[0]![1]!;
    expect(cell).toEqual([{ type: 'text', text: '• one\n• two\n• three' }]);
  });
  it('keeps <br> literal inside inline code', () => {
    expect(parseInline('`a<br>b`')).toEqual([{ type: 'code', text: 'a<br>b' }]);
  });
});
