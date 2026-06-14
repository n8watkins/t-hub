// A small, dependency-free Markdown renderer that builds React elements
// directly — no `dangerouslySetInnerHTML`, so arbitrary file contents can never
// inject HTML/script (the reader opens whatever file the user picks). It is a
// pragmatic subset, not a CommonMark implementation: headings, fenced + indented
// code blocks, blockquotes, unordered/ordered lists, horizontal rules, and the
// common inline spans (bold, italic, `code`, links, images-as-links). Anything
// it doesn't recognize falls back to plain text, which is the safe default.
//
// Used by FilePanel's reader for `.md`/`.markdown`. Other extensions render as
// monospace plain text (see FilePanel), so this file owns Markdown only.

import { type ReactNode } from "react";

/** Render Markdown `source` to a column of React elements. */
export function Markdown({ source }: { source: string }) {
  const blocks = parseBlocks(source);
  return (
    <div className="prose-tight max-w-none text-sm leading-relaxed text-neutral-200">
      {blocks.map((b, i) => (
        <Block key={i} block={b} />
      ))}
    </div>
  );
}

// --- Block model -----------------------------------------------------------

type Block =
  | { kind: "heading"; level: number; text: string }
  | { kind: "code"; lang: string; text: string }
  | { kind: "quote"; lines: string[] }
  | { kind: "list"; ordered: boolean; items: string[] }
  | { kind: "hr" }
  | { kind: "para"; text: string };

/** Split source into block-level chunks. Intentionally line-oriented. */
function parseBlocks(source: string): Block[] {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const blocks: Block[] = [];
  let i = 0;

  const isHr = (l: string) => /^\s{0,3}([-*_])(\s*\1){2,}\s*$/.test(l);

  while (i < lines.length) {
    const line = lines[i];

    // Blank line → block separator.
    if (line.trim() === "") {
      i++;
      continue;
    }

    // Fenced code block: ``` or ~~~ (optionally with a language).
    const fence = line.match(/^\s{0,3}(```+|~~~+)\s*([\w.-]*)\s*$/);
    if (fence) {
      const marker = fence[1][0];
      const lang = fence[2] ?? "";
      const body: string[] = [];
      i++;
      while (i < lines.length && !new RegExp(`^\\s{0,3}${marker}{3,}\\s*$`).test(lines[i])) {
        body.push(lines[i]);
        i++;
      }
      i++; // consume closing fence (if present)
      blocks.push({ kind: "code", lang, text: body.join("\n") });
      continue;
    }

    // Horizontal rule.
    if (isHr(line)) {
      blocks.push({ kind: "hr" });
      i++;
      continue;
    }

    // ATX heading: # .. ######
    const heading = line.match(/^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$/);
    if (heading) {
      blocks.push({ kind: "heading", level: heading[1].length, text: heading[2] });
      i++;
      continue;
    }

    // Blockquote: consecutive `>` lines.
    if (/^\s{0,3}>/.test(line)) {
      const qlines: string[] = [];
      while (i < lines.length && /^\s{0,3}>/.test(lines[i])) {
        qlines.push(lines[i].replace(/^\s{0,3}>\s?/, ""));
        i++;
      }
      blocks.push({ kind: "quote", lines: qlines });
      continue;
    }

    // Indented code block (4+ spaces). A blank line ends the block and is left
    // for the outer loop to skip as a separator.
    if (/^ {4}\S/.test(line)) {
      const body: string[] = [];
      while (i < lines.length && /^ {4}/.test(lines[i])) {
        body.push(lines[i].replace(/^ {4}/, ""));
        i++;
      }
      blocks.push({ kind: "code", lang: "", text: body.join("\n") });
      continue;
    }

    // List: a run of -/*/+ (unordered) or `N.` (ordered) items.
    const ulMatch = line.match(/^\s{0,3}[-*+]\s+(.*)$/);
    const olMatch = line.match(/^\s{0,3}\d+[.)]\s+(.*)$/);
    if (ulMatch || olMatch) {
      const ordered = !!olMatch;
      const items: string[] = [];
      const itemRe = ordered ? /^\s{0,3}\d+[.)]\s+(.*)$/ : /^\s{0,3}[-*+]\s+(.*)$/;
      while (i < lines.length) {
        const m = lines[i].match(itemRe);
        if (m) {
          items.push(m[1]);
          i++;
        } else if (/^\s+\S/.test(lines[i]) && items.length) {
          // Continuation line of the previous item.
          items[items.length - 1] += " " + lines[i].trim();
          i++;
        } else {
          break;
        }
      }
      blocks.push({ kind: "list", ordered, items });
      continue;
    }

    // Paragraph: gather until a blank line or a block-starting line.
    const para: string[] = [];
    while (
      i < lines.length &&
      lines[i].trim() !== "" &&
      !/^\s{0,3}(#{1,6})\s+/.test(lines[i]) &&
      !/^\s{0,3}(```+|~~~+)/.test(lines[i]) &&
      !/^\s{0,3}>/.test(lines[i]) &&
      !isHr(lines[i]) &&
      !/^\s{0,3}[-*+]\s+/.test(lines[i]) &&
      !/^\s{0,3}\d+[.)]\s+/.test(lines[i])
    ) {
      para.push(lines[i]);
      i++;
    }
    blocks.push({ kind: "para", text: para.join("\n") });
  }

  return blocks;
}

// --- Block rendering -------------------------------------------------------

function Block({ block }: { block: Block }) {
  switch (block.kind) {
    case "heading": {
      const sizes = [
        "text-2xl font-semibold mt-4 mb-2",
        "text-xl font-semibold mt-4 mb-2",
        "text-lg font-semibold mt-3 mb-1.5",
        "text-base font-semibold mt-3 mb-1.5",
        "text-sm font-semibold mt-2 mb-1",
        "text-sm font-semibold text-neutral-400 mt-2 mb-1",
      ];
      const cls = sizes[block.level - 1] ?? sizes[5];
      const border = block.level <= 2 ? " border-b border-neutral-800 pb-1" : "";
      return (
        <div className={`text-neutral-100 ${cls}${border}`}>
          <Inline text={block.text} />
        </div>
      );
    }
    case "code":
      return (
        <pre className="my-2 overflow-x-auto rounded-md border border-neutral-800 bg-neutral-900/70 p-3">
          <code className="font-mono text-[12.5px] leading-snug text-neutral-200">
            {block.text}
          </code>
        </pre>
      );
    case "quote":
      return (
        <blockquote className="my-2 border-l-2 border-neutral-700 pl-3 text-neutral-400">
          {block.lines.map((l, i) => (
            <p key={i} className="my-0.5">
              <Inline text={l} />
            </p>
          ))}
        </blockquote>
      );
    case "list": {
      const cls = "my-2 ml-5 space-y-0.5 " + (block.ordered ? "list-decimal" : "list-disc");
      return (
        <ul className={cls}>
          {block.items.map((it, i) => (
            <li key={i} className="text-neutral-200">
              <Inline text={it} />
            </li>
          ))}
        </ul>
      );
    }
    case "hr":
      return <hr className="my-4 border-neutral-800" />;
    case "para":
      return (
        <p className="my-2 text-neutral-200">
          <Inline text={block.text} />
        </p>
      );
  }
}

// --- Inline rendering ------------------------------------------------------

/**
 * Render inline Markdown spans within a single text run. Handles, in priority
 * order: inline `code`, images `![alt](url)` (rendered as a safe link), links
 * `[text](url)`, bold `**`/`__`, and italic `*`/`_`. Unrecognized text is plain.
 * Links are only rendered as anchors for http(s)/mailto schemes; anything else
 * is shown as plain text to avoid `javascript:`-style injection.
 */
function Inline({ text }: { text: string }): ReactNode {
  return <>{renderInline(text)}</>;
}

function renderInline(text: string): ReactNode[] {
  const out: ReactNode[] = [];
  let rest = text;
  let key = 0;

  // Ordered list of inline rules. Each tries to match at the current position
  // anywhere in `rest`; we take the earliest match across all rules.
  type Rule = {
    re: RegExp;
    make: (m: RegExpExecArray) => ReactNode;
  };

  const rules: Rule[] = [
    // Inline code (highest priority; its content is not further parsed).
    {
      re: /`([^`]+)`/,
      make: (m) => (
        <code
          key={key++}
          className="rounded bg-neutral-800 px-1 py-0.5 font-mono text-[12.5px] text-amber-200"
        >
          {m[1]}
        </code>
      ),
    },
    // Image → safe link (we don't load remote images in the reader).
    {
      re: /!\[([^\]]*)\]\(([^)\s]+)\)/,
      make: (m) => safeLink(m[2], m[1] || m[2], key++, true),
    },
    // Link.
    {
      re: /\[([^\]]+)\]\(([^)\s]+)\)/,
      make: (m) => safeLink(m[2], m[1], key++, false),
    },
    // Bold.
    {
      re: /\*\*([^*]+)\*\*|__([^_]+)__/,
      make: (m) => (
        <strong key={key++} className="font-semibold text-neutral-100">
          {renderInline(m[1] ?? m[2])}
        </strong>
      ),
    },
    // Italic (single * or _, not part of a ** run).
    {
      re: /\*([^*]+)\*|\b_([^_]+)_\b/,
      make: (m) => (
        <em key={key++} className="italic">
          {renderInline(m[1] ?? m[2])}
        </em>
      ),
    },
  ];

  // Iteratively consume `rest`, always applying the earliest-matching rule.
  // Bounded by input length (each step consumes ≥1 char).
  let guard = 0;
  while (rest.length > 0 && guard++ < 10000) {
    let best: { idx: number; len: number; node: ReactNode } | null = null;
    for (const rule of rules) {
      const m = rule.re.exec(rest);
      if (m && (best === null || m.index < best.idx)) {
        best = { idx: m.index, len: m[0].length, node: rule.make(m) };
      }
    }
    if (!best) {
      out.push(rest);
      break;
    }
    if (best.idx > 0) out.push(rest.slice(0, best.idx));
    out.push(best.node);
    rest = rest.slice(best.idx + best.len);
  }

  return out;
}

/** Render a link only if it uses a safe scheme; otherwise show plain text. */
function safeLink(href: string, label: string, key: number, isImage: boolean): ReactNode {
  const safe = /^(https?:|mailto:)/i.test(href);
  if (!safe) {
    return <span key={key}>{label}</span>;
  }
  return (
    <a
      key={key}
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      className="text-sky-400 underline decoration-sky-700 underline-offset-2 hover:text-sky-300"
      title={isImage ? `image: ${href}` : href}
    >
      {isImage ? `[image] ${label}` : label}
    </a>
  );
}
