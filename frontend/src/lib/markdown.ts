// Safe server-side Markdown rendering for untrusted agent output.
//
// - `html: false`: raw HTML in the source is escaped, never passed through.
// - markdown-it's default `validateLink` rejects javascript:, vbscript:, file:
//   and non-image data: URLs; we additionally allow only http(s), mailto and
//   in-page anchors.
// - Images are disabled: the CSP blocks remote images anyway, and agent output
//   must not trigger requests to arbitrary hosts.
// - Links open normally but carry rel="nofollow noopener noreferrer".
// The result is assigned with `set:html` only after this function.
import MarkdownIt from "markdown-it";

const SAFE_LINK = /^(https?:|mailto:|#)/i;

const md = new MarkdownIt({ html: false, linkify: true, typographer: false, breaks: false });
md.disable(["image"]);
md.validateLink = (url: string) => SAFE_LINK.test(url.trim());

const defaultLinkOpen =
  md.renderer.rules.link_open ?? ((tokens, idx, options, _env, self) => self.renderToken(tokens, idx, options));
md.renderer.rules.link_open = (tokens, idx, options, env, self) => {
  tokens[idx]!.attrSet("rel", "nofollow noopener noreferrer");
  return defaultLinkOpen(tokens, idx, options, env, self);
};

// Agent headings sit inside the page's own outline (h1 page, h2 section, h3
// draft title). The distinct levels used by the agent are renumbered from h4
// without gaps (`#`/`###` → h4/h5), capped at h6, so the outline never skips.
md.core.ruler.push("nest_headings", (state) => {
  const headings = state.tokens.filter((t) => t.type === "heading_open" || t.type === "heading_close");
  const levels = [...new Set(headings.map((t) => Number(t.tag.slice(1))))].sort((a, b) => a - b);
  for (const t of headings) t.tag = `h${Math.min(6, 4 + levels.indexOf(Number(t.tag.slice(1))))}`;
});

// Code blocks and tables scroll horizontally on narrow screens; make them
// keyboard-focusable so the overflow is reachable without a pointer (WCAG 2.1.1).
for (const rule of ["fence", "code_block"] as const) {
  const original = md.renderer.rules[rule]!;
  md.renderer.rules[rule] = (...args) => original(...args).replace(/^<pre/, '<pre tabindex="0"');
}
md.renderer.rules.table_open = (tokens, idx, options, _env, self) => {
  tokens[idx]!.attrSet("tabindex", "0");
  return self.renderToken(tokens, idx, options);
};

/** Renders Markdown to HTML that is safe to insert into the page. */
export function renderMarkdown(source: string): string {
  return md.render(source);
}
