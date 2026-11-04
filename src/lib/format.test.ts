import { describe, expect, it } from "vitest";
import { esc, gb, inline, md, mdText, modelLabel, pct, short } from "./format";

describe("esc", () => {
  it("escapes every HTML special character", () => {
    expect(esc(`<a href="x" onclick='y'>&</a>`)).toBe(
      "&lt;a href=&quot;x&quot; onclick=&#39;y&#39;&gt;&amp;&lt;/a&gt;",
    );
  });
  it("leaves plain text alone", () => {
    expect(esc("hello world 123")).toBe("hello world 123");
  });
});

describe("gb", () => {
  it("shows one decimal below 10 GB and none above", () => {
    expect(gb(512)).toBe("0.5 GB");
    expect(gb(2048)).toBe("2.0 GB");
    expect(gb(10240)).toBe("10 GB");
    expect(gb(20480)).toBe("20 GB");
  });
});

describe("pct", () => {
  it("computes a percentage and clamps to 0-100", () => {
    expect(pct(1, 4)).toBe(25);
    expect(pct(10, 4)).toBe(100);
    expect(pct(-5, 4)).toBe(0);
  });
  it("is 0 when the total is not positive", () => {
    expect(pct(5, 0)).toBe(0);
    expect(pct(5, -1)).toBe(0);
  });
});

describe("short", () => {
  it("keeps short strings and truncates long ones with an ellipsis", () => {
    expect(short("abc", 5)).toBe("abc");
    expect(short("abcde", 5)).toBe("abcde");
    expect(short("abcdef", 5)).toBe("abcd…");
  });
});

describe("modelLabel", () => {
  it("drops the extension and the quantization suffix", () => {
    expect(modelLabel("Llama-3.2-1B-Instruct-Q4_K_M.gguf")).toBe("Llama-3.2-1B-Instruct");
    expect(modelLabel("gemma-3-1b-it-Q4_0.GGUF")).toBe("gemma-3-1b-it");
    expect(modelLabel("custom.gguf")).toBe("custom");
  });
});

describe("inline markdown", () => {
  it("renders code and bold", () => {
    expect(inline("use `ls` and **bold**")).toBe("use <code>ls</code> and <strong>bold</strong>");
  });
  it("escapes HTML so model output cannot inject markup", () => {
    expect(inline("<img src=x onerror=alert(1)>")).not.toContain("<img");
    expect(inline("**<b>x</b>**")).toBe("<strong>&lt;b&gt;x&lt;/b&gt;</strong>");
  });
});

describe("block markdown (mdText)", () => {
  it("makes paragraphs from lines", () => {
    expect(mdText("one\ntwo")).toBe("<p>one</p><p>two</p>");
  });
  it("renders bullet and numbered lists", () => {
    expect(mdText("- a\n- b")).toBe("<ul><li>a</li><li>b</li></ul>");
    expect(mdText("1. a\n2) b")).toBe("<ol><li>a</li><li>b</li></ol>");
  });
  it("keeps one list (and its numbering) across blank lines between items", () => {
    expect(mdText("1. a\n\n2. b\n\n3. c")).toBe("<ol><li>a</li><li>b</li><li>c</li></ol>");
  });
  it("closes a list when normal text follows", () => {
    expect(mdText("- a\ntext")).toBe("<ul><li>a</li></ul><p>text</p>");
  });
  it("turns headings into bold paragraphs", () => {
    expect(mdText("## Title")).toBe("<p><strong>Title</strong></p>");
  });
  it("returns nothing for empty input", () => {
    expect(mdText("")).toBe("");
    expect(mdText("\n\n")).toBe("");
  });
});

describe("md (with code blocks)", () => {
  it("renders a fenced block with its language and a copy button", () => {
    const html = md("Before\n```python\nprint('hi')\n```\nAfter");
    expect(html).toContain('<div class="codeblock">');
    expect(html).toContain("<span>python</span>");
    expect(html).toContain('data-act="copy-code"');
    expect(html).toContain("print(&#39;hi&#39;)");
    expect(html).toContain("<p>Before</p>");
    expect(html).toContain("<p>After</p>");
  });
  it("labels a block without a language as code", () => {
    expect(md("```\nx\n```")).toContain("<span>code</span>");
  });
  it("escapes HTML inside code blocks", () => {
    const html = md("```html\n<script>alert(1)</script>\n```");
    expect(html).not.toContain("<script>");
    expect(html).toContain("&lt;script&gt;");
  });
  it("still renders while a code block is unfinished (streaming)", () => {
    const html = md("Here:\n```js\nconst a = 1;");
    expect(html).toContain("const a = 1;");
    expect(html).toContain('class="codeblock"');
  });
});
