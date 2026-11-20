import assert from "node:assert/strict";
import { formatKeyHint } from "./keyHint";

const cases = [
  ["ctrl+enter", "⌃↩"],
  ["command+shift+k", "⇧⌘K"],
  ["option_left+escape", "⌥esc"],
  ["ctrl+escape", "⌃esc"],
  ["ctrl+pageup", "⌃⇞"],
  ["ctrl+pagedown", "⌃⇟"],
  ["ctrl+home", "⌃↖"],
  ["ctrl+end", "⌃↘"],
  ["ctrl+tab", "⌃⇥"],
  ["ctrl+space", "⌃␣"],
  ["ctrl+delete", "⌃⌦"],
  ["ctrl+backspace", "⌃⌫"],
  ["ctrl+arrowleft", "⌃←"],
  ["ctrl+f5", "⌃F5"],
  ["ctrl+f20", "⌃F20"],
  ["ctrl+insert", "⌃Ins"],
  ["ctrl+numpad 5", "⌃Numpad 5"],
  ["", ""],
] as const;

for (const [binding, expected] of cases) {
  assert.equal(formatKeyHint(binding), expected, binding);
}

console.log("keyHint tests passed");
