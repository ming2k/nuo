/**
 * Markdown rendering for transcript content.
 *
 * Content comes from the model and tool output — untrusted by policy, so it
 * must never reach `{@html}` unfiltered. The pipeline is:
 *
 *   marked (GFM) → DOMPurify (industry-standard XSS sanitization) →
 *   highlight.js (code-block syntax colouring, post-sanitize so its spans
 *   never widen the attack surface) → link hardening.
 *
 * Rendering is synchronous (`async: false`), which also pins
 * `marked.parse`'s `string | Promise<string>` return to `string`.
 */

import { marked } from "marked";
import DOMPurify from "dompurify";
import hljs from "highlight.js/lib/common";

marked.setOptions({ breaks: true, gfm: true, async: false });

export function renderMarkdown(source: string): string {
  const raw = marked.parse(source ?? "") as string;
  // DOMPurify's default profile strips script/iframe/object/embed/form and
  // every on* handler, and constrains href/src to safe schemes (http, https,
  // mailto, relative…). We additionally forbid <style> (CSS is layout attack
  // surface for prose) and interactive form controls.
  const clean = DOMPurify.sanitize(raw, {
    RETURN_DOM: true,
    FORBID_TAGS: ["style", "form", "input", "button", "select", "textarea"],
  }) as HTMLElement;

  for (const block of Array.from(clean.querySelectorAll("pre code"))) {
    try {
      hljs.highlightElement(block as HTMLElement);
    } catch {
      // Unknown language / malformed class — leave the block plain.
    }
  }

  // Any link that survived sanitization opens externally and safely.
  for (const anchor of Array.from(clean.querySelectorAll("a[href]"))) {
    anchor.setAttribute("target", "_blank");
    anchor.setAttribute("rel", "noopener noreferrer");
  }

  return clean.innerHTML;
}
