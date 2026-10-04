// Compact key hints for the learned-correction toast buttons (fork feature:
// voice-control): "ctrl+enter" → "⌃↩". Mac glyphs, because the toast only
// fires on macOS (the learning session is macOS only).

// Display order of modifiers, as macOS menus show them.
const MODIFIER_GLYPHS: [string[], string][] = [
  [["fn", "function"], "fn"],
  [["ctrl", "control"], "⌃"],
  [["option", "alt", "opt"], "⌥"],
  [["shift"], "⇧"],
  [["command", "cmd", "meta", "super"], "⌘"],
];

const KEY_GLYPHS: Record<string, string> = {
  enter: "↩",
  return: "↩",
  numpadenter: "⌤",
  keypadenter: "⌤",
  backspace: "⌫",
  delete: "⌦",
  escape: "⎋",
  esc: "⎋",
  tab: "⇥",
  space: "␣",
  up: "↑",
  down: "↓",
  left: "←",
  right: "→",
  arrowup: "↑",
  arrowdown: "↓",
  arrowleft: "←",
  arrowright: "→",
  pageup: "⇞",
  pagedown: "⇟",
  home: "↖",
  end: "↘",
  capslock: "⇪",
};

// Named keys without a common glyph read as words, not as "PRINTSCREEN".
const KEY_WORDS: Record<string, string> = {
  insert: "Ins",
  printscreen: "Print",
  scrolllock: "Scroll Lock",
  numlock: "Num Lock",
  pause: "Pause",
};

const formatKey = (key: string): string => {
  const glyph = KEY_GLYPHS[key] ?? KEY_WORDS[key];
  if (glyph) return glyph;
  // "k" → "K", "f5" → "F5", "numpad 5" → "Numpad 5".
  return /^f\d+$/.test(key)
    ? key.toUpperCase()
    : key.replace(/\b\w/g, (c) => c.toUpperCase());
};

const stripSide = (part: string) => part.replace(/_(left|right)$/, "");

export const formatKeyHint = (binding: string): string => {
  const parts = binding
    .toLowerCase()
    .split("+")
    .map((part) => stripSide(part.trim()))
    .filter(Boolean);
  const modifiers = MODIFIER_GLYPHS.filter(([names]) =>
    parts.some((part) => names.includes(part)),
  ).map(([, glyph]) => glyph);
  const isModifier = (part: string) =>
    MODIFIER_GLYPHS.some(([names]) => names.includes(part));
  const keys = parts.filter((part) => !isModifier(part)).map(formatKey);
  return [...modifiers, ...keys].join("");
};
