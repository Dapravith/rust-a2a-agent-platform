import assert from "node:assert/strict";
import { test } from "node:test";
import { renderMarkdown } from "../../src/lib/markdown.ts";

test("raw HTML is escaped, never passed through", () => {
  const html = renderMarkdown(`<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n<a href="javascript:alert(1)">x</a>`);
  assert.doesNotMatch(html, /<script|<img|<a /i);
  assert.match(html, /&lt;script&gt;/);
});

test("dangerous link schemes are not linked", () => {
  for (const url of ["javascript:alert(1)", "JaVaScRiPt:alert(1)", "vbscript:x", "data:text/html,<b>x</b>", "file:///etc/passwd", " javascript:alert(1)"]) {
    const html = renderMarkdown(`[x](${url})`);
    assert.doesNotMatch(html, /<a /, url);
  }
});

test("safe links get rel=nofollow noopener noreferrer", () => {
  const html = renderMarkdown("[docs](https://example.com) and https://example.org");
  assert.equal((html.match(/rel="nofollow noopener noreferrer"/g) ?? []).length, 2);
});

test("images are not rendered (no requests to arbitrary hosts)", () => {
  const html = renderMarkdown("![pixel](https://tracker.example/p.png)");
  assert.doesNotMatch(html, /<img/);
});

test("headings are nested below the page outline without skipping levels", () => {
  const html = renderMarkdown("# One\n\n## Two\n\n#### Four\n\n##### Five");
  assert.match(html, /<h4>One<\/h4>/);
  assert.match(html, /<h5>Two<\/h5>/);
  assert.match(html, /<h6>Four<\/h6>/);
  assert.match(html, /<h6>Five<\/h6>/);
  assert.doesNotMatch(html, /<h[1-3]>/);
  // A document that starts at `##` still begins at h4.
  assert.match(renderMarkdown("## Fields\n\n### Sub"), /<h4>Fields<\/h4>[\s\S]*<h5>Sub<\/h5>/);
});

test("scrollable code blocks and tables are keyboard-focusable", () => {
  assert.match(renderMarkdown("```\ncode\n```"), /^<pre tabindex="0">/);
  assert.match(renderMarkdown("    indented code"), /^<pre tabindex="0">/);
  assert.match(renderMarkdown("| a |\n|---|\n| b |"), /<table tabindex="0">/);
});

test("code blocks keep their content escaped", () => {
  const html = renderMarkdown("```html\n<div onclick=\"x()\">\n```");
  assert.match(html, /&lt;div onclick=&quot;x\(\)&quot;&gt;/);
});
