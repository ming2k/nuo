// @vitest-environment jsdom
// DOMPurify needs real DOM semantics: happy-dom executes <script> inserted via
// innerHTML (so sanitization throws mid-parse) and mis-handles template
// parsing — jsdom is the supported fidelity target for sanitizer tests.
import { describe, expect, it } from "vitest";
import { renderMarkdown } from "./markdown.js";

describe("renderMarkdown sanitization", () => {
  it("strips <script> and <style> blocks", () => {
    const html = renderMarkdown(
      "safe <script>alert(1)</script> <style>body{display:none}</style>",
    );
    expect(html).toContain("safe");
    expect(html).not.toContain("<script");
    expect(html).not.toContain("alert(1)");
    expect(html).not.toContain("<style");
    expect(html).not.toContain("display:none");
  });

  it("strips on* handler attributes from raw HTML", () => {
    const html = renderMarkdown(
      '<img src="x.png" onerror="alert(1)"> <a href="https://example.com" onclick="alert(2)">link</a>',
    );
    expect(html).toContain("<img");
    expect(html).not.toContain("onerror");
    expect(html).not.toContain("onclick");
    expect(html).not.toContain("alert(");
  });

  it("drops javascript: hrefs but keeps http and relative links", () => {
    const html = renderMarkdown(
      "[evil](javascript:alert(1)) [ok](http://example.com) [rel](./docs)",
    );
    expect(html).not.toContain("javascript:");
    // The anchor text survives even though its href was removed.
    expect(html).toContain("evil");
    expect(html).toContain('href="http://example.com"');
    expect(html).toContain('href="./docs"');
  });

  it("hardens surviving anchors with target and rel", () => {
    const html = renderMarkdown("[ok](http://example.com)");
    expect(html).toContain('target="_blank"');
    expect(html).toContain('rel="noopener noreferrer"');
  });

  it("highlights fenced code blocks with highlight.js markup", () => {
    const html = renderMarkdown("```js\nconst x = 1;\n```");
    expect(html).toContain("hljs");
  });
});
