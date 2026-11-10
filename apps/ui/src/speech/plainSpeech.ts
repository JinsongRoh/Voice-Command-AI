/**
 * Text written for the screen (Markdown from an agent) as a voice should
 * read it: no list bullets, emphasis stars, heading marks, table bars,
 * code fences or link targets. Line structure becomes sentence pauses.
 */
export function plainSpeech(text: string): string {
  const lines = text
    // Code blocks are not read; a voice reading code helps nobody.
    .replace(/```[\s\S]*?(```|$)/g, '\n')
    .split(/\r?\n/)
    .map((raw) => {
      let line = raw.trim();
      // Horizontal rules and table separator rows.
      if (/^([-*_=]\s*){3,}$/.test(line) || /^\|?[\s:|-]+\|[\s:|-]*$/.test(line)) return '';
      line = line
        .replace(/^>+\s*/, '') // blockquote
        .replace(/^#{1,6}\s+/, '') // heading
        .replace(/^[-*+•·]\s+(\[[ xX]\]\s+)?/, '') // bullet, task box
        .replace(/^\|(.*)\|$/, '$1') // table row edges
        .replace(/\s*\|\s*/g, ', ') // table cells
        .replace(/!\[([^\]]*)\]\([^)]*\)/g, '$1') // image
        .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1') // link -> its text
        .replace(/<?https?:\/\/[^\s>)]+>?/g, '') // bare URL
        .replace(/`([^`]*)`/g, '$1') // inline code
        .replace(/(\*\*|__)(.+?)\1/g, '$2') // bold
        .replace(/(^|[^\w*])[*_]([^*_\s][^*_]*?)[*_](?=[^\w*]|$)/g, '$1$2') // italic
        .replace(/~~(.+?)~~/g, '$1') // strikethrough
        .replace(/[*_~`#]+/g, ' ') // stray marks
        .replace(/\s+[-–—]+\s+/g, ', ') // " - " used as a separator
        .replace(/\s*(->|=>|→|›)\s*/g, ', ') // arrows
        .replace(/\s{2,}/g, ' ')
        .trim();
      return line;
    })
    .filter((l) => l.length > 0);
  // Each line ends a sentence so the voice pauses between list items.
  return lines.map((l) => (/[.!?。…:,]$/.test(l) ? l : `${l}.`)).join(' ');
}
