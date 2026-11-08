import { pathToFileURL } from 'node:url';

export const RESET = '\x1b[0m';
export const BOLD = '\x1b[1m';
export const DIM = '\x1b[2m';
export const ITALIC = '\x1b[3m';

export const COLORS = {
  reset: RESET,
  bold: BOLD,
  dim: DIM,
  gray: '\x1b[90m',
  red: '\x1b[31m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  magenta: '\x1b[35m',
  cyan: '\x1b[36m',
  white: '\x1b[37m',
  brightRed: '\x1b[91m',
  brightGreen: '\x1b[92m',
  brightYellow: '\x1b[93m',
  brightBlue: '\x1b[94m',
  brightMagenta: '\x1b[95m',
  brightCyan: '\x1b[96m',
  brightWhite: '\x1b[97m',
} as const;

export type ColorName = keyof typeof COLORS;

// Matches CSI sequences, OSC sequences (including OSC 8 links), and stray ESC sequences.
const ANSI_REGEX =
  /\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[ -/]*[0-~]/g;

// Control characters and Unicode bidi override/isolate characters (Trojan Source defense).
const UNSAFE_TEXT_REGEX =
  /[\x00-\x1f\x7f-\x9f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/g;

export function sanitizeText(input: unknown): string {
  if (typeof input !== 'string') return '';
  return input.replace(ANSI_REGEX, '').replace(UNSAFE_TEXT_REGEX, ' ').replace(/\s+/g, ' ').trim();
}

export function stripAnsi(text: string): string {
  return String(text).replace(ANSI_REGEX, '');
}

/**
 * Computes terminal display column width of a single Unicode code point ( East Asian Wide / Emoji = 2 ).
 */
export function codePointWidth(cp: number): number {
  if (cp <= 0x1f || (cp >= 0x7f && cp <= 0x9f)) return 0;
  // Combining marks & variation selectors & zero-width joiners
  if (
    (cp >= 0x0300 && cp <= 0x036f) ||
    (cp >= 0x1ab0 && cp <= 0x1aff) ||
    (cp >= 0x200b && cp <= 0x200f) ||
    (cp >= 0xfe00 && cp <= 0xfe0f) ||
    (cp >= 0xe0100 && cp <= 0xe01ef)
  ) {
    return 0;
  }
  // Wide CJK, Hangul, Fullwidth, and common Emoji ranges
  if (
    cp >= 0x1100 &&
    (cp <= 0x115f ||
      cp === 0x2329 ||
      cp === 0x232a ||
      (cp >= 0x2e80 && cp <= 0x303e) ||
      (cp >= 0x3040 && cp <= 0xa4cf && cp !== 0x303f) ||
      (cp >= 0xac00 && cp <= 0xd7a3) ||
      (cp >= 0xf900 && cp <= 0xfaff) ||
      (cp >= 0xfe10 && cp <= 0xfe19) ||
      (cp >= 0xfe30 && cp <= 0xfe6f) ||
      (cp >= 0xff00 && cp <= 0xff60) ||
      (cp >= 0xffe0 && cp <= 0xffe6) ||
      (cp >= 0x1f300 && cp <= 0x1faff) ||
      (cp >= 0x20000 && cp <= 0x3fffd))
  ) {
    return 2;
  }
  return 1;
}

export function textWidth(text: string): number {
  const plain = stripAnsi(text);
  let width = 0;
  for (const ch of plain) {
    const cp = ch.codePointAt(0);
    if (cp !== undefined) width += codePointWidth(cp);
  }
  return width;
}

/**
 * Truncates an ANSI-styled string to fit within `maxCols` columns, preserving SGR resets
 * and closing any open OSC 8 hyperlink cleanly.
 */
export function truncateAnsi(text: string, maxCols: number, ellipsis = '…'): string {
  if (maxCols <= 0) return '';
  if (textWidth(text) <= maxCols) return text;

  const ellipsisWidth = textWidth(ellipsis);
  const budget = Math.max(0, maxCols - ellipsisWidth);

  const tokenRegex =
    /(\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[ -/]*[0-~])/g;

  let out = '';
  let used = 0;
  let openOsc8 = false;
  let lastIndex = 0;

  const appendVisible = (segment: string): boolean => {
    for (const ch of segment) {
      const cp = ch.codePointAt(0) ?? 0;
      const w = codePointWidth(cp);
      if (used + w > budget) return false;
      out += ch;
      used += w;
    }
    return true;
  };

  for (const match of text.matchAll(tokenRegex)) {
    const idx = match.index ?? 0;
    if (idx > lastIndex) {
      if (!appendVisible(text.slice(lastIndex, idx))) {
        return `${out}${openOsc8 ? '\x1b]8;;\x1b\\' : ''}${RESET}${budget >= 0 && maxCols >= ellipsisWidth ? ellipsis : ''}`;
      }
    }
    const seq = match[0];
    if (seq.startsWith('\x1b]8;;')) {
      openOsc8 = seq !== '\x1b]8;;\x1b\\' && seq !== '\x1b]8;;\x07';
    }
    out += seq;
    lastIndex = idx + seq.length;
  }

  if (lastIndex < text.length) {
    appendVisible(text.slice(lastIndex));
  }

  const closeLink = openOsc8 ? '\x1b]8;;\x1b\\' : '';
  return `${out}${closeLink}${RESET}${maxCols >= ellipsisWidth ? ellipsis : ''}`;
}

export function paint(text: string, ...codes: string[]): string {
  if (!text) return '';
  return `${codes.join('')}${text}${RESET}`;
}

/**
 * Builds a fine-grained Unicode progress bar (`█▓▒░·`) inspired by Antigravity & claude-hud.
 */
export function renderBar(
  percent: number,
  length = 10,
  warningAt = 60,
  criticalAt = 85,
  normalColor: string = COLORS.brightGreen,
): string {
  const clamped = Math.max(0, Math.min(100, Number.isFinite(percent) ? percent : 0));
  const exact = (clamped * length) / 100;
  const full = Math.floor(exact);
  const rem = (exact - full) * 100;

  let barColor = normalColor;
  if (clamped >= criticalAt) barColor = COLORS.brightRed;
  else if (clamped >= warningAt) barColor = COLORS.brightYellow;

  let filledPart = '';
  let emptyPart = '';
  for (let i = 0; i < length; i++) {
    if (i < full) {
      filledPart += '█';
    } else if (i === full && clamped > 0) {
      if (rem >= 75) filledPart += '▓';
      else if (rem >= 50) filledPart += '▒';
      else if (rem >= 25) filledPart += '░';
      else emptyPart += '·';
    } else {
      emptyPart += '·';
    }
  }

  return `${barColor}${filledPart}${COLORS.gray}${emptyPart}${RESET}`;
}

/**
 * Formats an OSC 8 terminal hyperlink to a local file path (`file://`), permitted by `terminalFilter`.
 */
export function fileLink(absPath: string, label: string, enabled = true): string {
  if (!enabled || !absPath) return label;
  try {
    const href = pathToFileURL(absPath).href;
    return `\x1b]8;;${href}\x1b\\${label}\x1b]8;;\x1b\\`;
  } catch {
    return label;
  }
}
