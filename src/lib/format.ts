// Pure helpers: text escaping, number formatting and a small, safe Markdown renderer.
import { ic } from "./icons";

/** Escapes text for safe use inside HTML (including attribute values). */
export const esc = (s: string): string =>
  s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);

/** Megabytes as a short "x.y GB" string. */
export const gb = (mb: number): string => (mb / 1024).toFixed(mb < 10240 ? 1 : 0) + " GB";

/** `a` as a percentage of `b`, clamped to 0–100 (0 when `b` is not positive). */
export const pct = (a: number, b: number): number => (b > 0 ? Math.min(100, Math.max(0, (a / b) * 100)) : 0);

/** Truncates to `n` characters, ending with an ellipsis. */
export const short = (s: string, n: number): string => (s.length > n ? s.slice(0, n - 1) + "…" : s);

/** A friendly model name from its file name (drops ".gguf" and the quantization suffix). */
export const modelLabel = (file: string): string => file.replace(/\.gguf$/i, "").replace(/-Q\d.*$/i, "");

/** Inline Markdown: `code` and **bold**. Everything else is escaped. */
export function inline(s: string): string {
  return esc(s)
    .replace(/`([^`\n]+)`/g, "<code>$1</code>")
    .replace(/\*\*([^*\n]+)\*\*/g, "<strong>$1</strong>");
}

/** Block Markdown without code fences: paragraphs, lists and headings. */
export function mdText(src: string): string {
  const out: string[] = [];
  let list: "" | "ul" | "ol" = "";
  const close = () => {
    if (list) {
      out.push(`</${list}>`);
      list = "";
    }
  };
  for (const raw of src.split("\n")) {
    const line = raw.trimEnd();
    let m: RegExpExecArray | null;
    if ((m = /^\s*[-*]\s+(.*)$/.exec(line))) {
      if (list !== "ul") {
        close();
        out.push("<ul>");
        list = "ul";
      }
      out.push(`<li>${inline(m[1])}</li>`);
    } else if ((m = /^\s*\d+[.)]\s+(.*)$/.exec(line))) {
      if (list !== "ol") {
        close();
        out.push("<ol>");
        list = "ol";
      }
      out.push(`<li>${inline(m[1])}</li>`);
    } else if ((m = /^#{1,4}\s+(.*)$/.exec(line))) {
      close();
      out.push(`<p><strong>${inline(m[1])}</strong></p>`);
    } else if (line.trim() === "") {
      // A blank line between list items keeps the list (and its numbering) open.
    } else {
      close();
      out.push(`<p>${inline(line)}</p>`);
    }
  }
  close();
  return out.join("");
}

/** Renders model output to HTML: Markdown text plus fenced code blocks with a copy button. */
export function md(src: string): string {
  return src
    .split(/```/)
    .map((part, i) => {
      if (i % 2 === 0) return mdText(part);
      const nl = part.indexOf("\n");
      const lang = nl > -1 ? part.slice(0, nl).trim() : "";
      const body = (nl > -1 ? part.slice(nl + 1) : part).replace(/\n$/, "");
      return `<div class="codeblock"><div class="ch"><span>${esc(lang || "code")}</span><button class="ibtn" data-act="copy-code" title="Copy">${ic("copy")}</button></div><pre><code>${esc(body)}</code></pre></div>`;
    })
    .join("");
}
