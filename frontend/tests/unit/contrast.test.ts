// WCAG 2.2 AA contrast for the design-token pairs components actually use,
// read from src/styles/tokens.css for both themes (text 4.5:1, UI 3:1).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const css = readFileSync(new URL("../../src/styles/tokens.css", import.meta.url), "utf8");
const darkStart = css.indexOf("@media (prefers-color-scheme: dark)");

function tokens(block: string): Record<string, string> {
  return Object.fromEntries([...block.matchAll(/--([\w-]+):\s*(#[0-9a-f]{6})\s*;/gi)].map((m) => [m[1]!, m[2]!.toLowerCase()]));
}
const light = tokens(css.slice(0, darkStart));
const dark = { ...light, ...tokens(css.slice(darkStart)) };

function luminance(hex: string): number {
  const [r, g, b] = [1, 3, 5].map((i) => {
    const c = parseInt(hex.slice(i, i + 2), 16) / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r! + 0.7152 * g! + 0.0722 * b!;
}
export function ratio(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi! + 0.05) / (lo! + 0.05);
}

// [foreground, background, minimum]
const PAIRS: [string, string, number][] = [
  ["color-text", "color-canvas", 4.5],
  ["color-text", "color-surface", 4.5],
  ["color-text-secondary", "color-surface", 4.5],
  ["color-text-secondary", "color-canvas", 4.5],
  ["color-text-secondary", "color-surface-sunken", 4.5],
  ["color-text-secondary", "color-surface-subtle", 4.5],
  ["color-text-placeholder", "color-surface", 4.5],
  ["color-accent-text", "color-surface", 4.5],
  ["color-accent-text", "color-accent-subtle", 4.5],
  ["color-on-accent", "color-accent", 4.5],
  ["color-on-accent", "color-accent-hover", 4.5],
  ["color-success-text", "color-success-bg", 4.5],
  ["color-warning-text", "color-warning-bg", 4.5],
  ["color-error-text", "color-error-bg", 4.5],
  ["color-error-text", "color-surface", 4.5],
  ["color-info-text", "color-info-bg", 4.5],
  ["color-neutral-text", "color-neutral-bg", 4.5],
  ["color-text", "color-warning-bg", 4.5],
  ["color-text", "color-error-bg", 4.5],
  ["color-success-fg", "color-surface", 3],
  ["color-warning-fg", "color-surface", 3],
  ["color-error-fg", "color-surface", 3],
  ["color-info-fg", "color-surface", 3],
  ["focus-ring-color", "color-surface", 3],
  ["focus-ring-color", "color-canvas", 3],
  ["color-border-strong", "color-surface", 1.4],
];

for (const [name, theme] of [["light", light], ["dark", dark]] as const) {
  test(`${name} theme token contrast`, () => {
    const failures = PAIRS.flatMap(([fg, bg, min]) => {
      assert.ok(theme[fg] && theme[bg], `missing token ${fg} or ${bg}`);
      const r = ratio(theme[fg]!, theme[bg]!);
      return r + 1e-9 < min ? [`${fg} on ${bg}: ${r.toFixed(2)} < ${min}`] : [];
    });
    assert.deepEqual(failures, []);
  });
}
