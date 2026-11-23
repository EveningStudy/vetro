import { CheckIcon, CopyIcon } from "lucide-react";
import { type Token, type Tokens, Lexer } from "marked";
import { Fragment, memo, type ReactNode, useMemo, useState } from "react";
import { useT } from "~/i18n";
import { cn, copyText } from "~/lib/utils";
import { useHighlighter } from "./highlight";

function CodeBlock({ code, lang }: { code: string; lang?: string }) {
  const t = useT();
  const [copied, setCopied] = useState(false);
  const highlight = useHighlighter();
  const html = useMemo(() => highlight?.(code, lang) ?? null, [highlight, code, lang]);
  return (
    <div className="chat-markdown-codeblock group/code relative">
      <div className="flex h-8 items-center justify-between border-b ps-3 pe-1 text-muted-foreground text-xs">
        <span className="font-mono">{lang || "text"}</span>
        <button
          type="button"
          aria-label={t("common.copy")}
          onClick={() => {
            void copyText(code).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1200);
            });
          }}
          className="inline-flex size-6 items-center justify-center rounded-md opacity-70 hover:bg-accent hover:opacity-100"
        >
          {copied ? <CheckIcon className="size-3.5" /> : <CopyIcon className="size-3.5" />}
        </button>
      </div>
      <pre>{html ? <code dangerouslySetInnerHTML={{ __html: html }} /> : <code>{code}</code>}</pre>
    </div>
  );
}

function safeHref(href: string) {
  return /^(https?:|mailto:|#)/i.test(href) ? href : undefined;
}

function inline(tokens: Token[] | undefined, keyPrefix = ""): ReactNode {
  if (!tokens) return null;
  return tokens.map((token, index) => {
    const key = `${keyPrefix}${index}`;
    switch (token.type) {
      case "text":
        return "tokens" in token && token.tokens ? <Fragment key={key}>{inline(token.tokens, `${key}.`)}</Fragment> : <Fragment key={key}>{decode(token.text)}</Fragment>;
      case "escape":
        return <Fragment key={key}>{decode(token.text)}</Fragment>;
      case "strong":
        return <strong key={key}>{inline((token as Tokens.Strong).tokens, `${key}.`)}</strong>;
      case "em":
        return <em key={key}>{inline((token as Tokens.Em).tokens, `${key}.`)}</em>;
      case "del":
        return <del key={key}>{inline((token as Tokens.Del).tokens, `${key}.`)}</del>;
      case "codespan":
        return <code key={key}>{decode(token.text)}</code>;
      case "br":
        return <br key={key} />;
      case "link": {
        const link = token as Tokens.Link;
        const href = safeHref(link.href);
        return href ? (
          <a key={key} href={href} target="_blank" rel="noreferrer noopener">
            {inline(link.tokens, `${key}.`)}
          </a>
        ) : (
          <code key={key}>{link.text}</code>
        );
      }
      case "image": {
        const image = token as Tokens.Image;
        return <span key={key} className="text-muted-foreground">[{image.text || image.href}]</span>;
      }
      case "html":
        return <Fragment key={key}>{token.raw}</Fragment>;
      default:
        return <Fragment key={key}>{token.raw}</Fragment>;
    }
  });
}

const entities: Record<string, string> = { "&amp;": "&", "&lt;": "<", "&gt;": ">", "&quot;": '"', "&#39;": "'" };
function decode(text: string) {
  return text.replace(/&(amp|lt|gt|quot|#39);/g, (match) => entities[match] ?? match);
}

function blocks(tokens: Token[], keyPrefix = ""): ReactNode {
  return tokens.map((token, index) => {
    const key = `${keyPrefix}${index}`;
    switch (token.type) {
      case "space":
        return null;
      case "paragraph":
        return <p key={key}>{inline((token as Tokens.Paragraph).tokens, `${key}.`)}</p>;
      case "heading": {
        const heading = token as Tokens.Heading;
        const Tag = `h${Math.min(6, heading.depth + 1)}` as "h2";
        return <Tag key={key}>{inline(heading.tokens, `${key}.`)}</Tag>;
      }
      case "code": {
        const code = token as Tokens.Code;
        return <CodeBlock key={key} code={code.text} lang={code.lang?.split(/\s/)[0]} />;
      }
      case "blockquote":
        return <blockquote key={key}>{blocks((token as Tokens.Blockquote).tokens, `${key}.`)}</blockquote>;
      case "hr":
        return <hr key={key} />;
      case "list": {
        const list = token as Tokens.List;
        const items = list.items.map((item, itemIndex) => (
          <li key={itemIndex} className={item.task ? "task-list-item" : undefined}>
            {item.task ? <input type="checkbox" checked={!!item.checked} readOnly /> : null}
            {item.loose ? blocks(item.tokens, `${key}.${itemIndex}.`) : listItemInline(item.tokens, `${key}.${itemIndex}.`)}
          </li>
        ));
        return list.ordered ? (
          <ol key={key} start={list.start === "" ? undefined : Number(list.start)}>
            {items}
          </ol>
        ) : (
          <ul key={key}>{items}</ul>
        );
      }
      case "table": {
        const table = token as Tokens.Table;
        return (
          <div key={key} className="chat-markdown-table">
            <table>
              <thead>
                <tr>
                  {table.header.map((cell, cellIndex) => (
                    <th key={cellIndex} style={{ textAlign: cell.align ?? undefined }}>
                      {inline(cell.tokens)}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {table.rows.map((row, rowIndex) => (
                  <tr key={rowIndex}>
                    {row.map((cell, cellIndex) => (
                      <td key={cellIndex} style={{ textAlign: cell.align ?? undefined }}>
                        {inline(cell.tokens)}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        );
      }
      case "html":
        return <p key={key}>{token.raw}</p>;
      case "text": {
        const text = token as Tokens.Text;
        return <p key={key}>{text.tokens ? inline(text.tokens, `${key}.`) : decode(text.text)}</p>;
      }
      default:
        return <p key={key}>{token.raw}</p>;
    }
  });
}

function listItemInline(tokens: Token[], keyPrefix: string): ReactNode {
  return tokens.map((token, index) => {
    if (token.type === "text") {
      const text = token as Tokens.Text;
      return <Fragment key={index}>{text.tokens ? inline(text.tokens, `${keyPrefix}${index}.`) : decode(text.text)}</Fragment>;
    }
    return <Fragment key={index}>{blocks([token], `${keyPrefix}${index}.`)}</Fragment>;
  });
}

export const Markdown = memo(function Markdown({ text, className }: { text: string; className?: string }) {
  const tokens = useMemo(() => Lexer.lex(text, { gfm: true, breaks: true }), [text]);
  return <div className={cn("chat-markdown", className)}>{blocks(tokens)}</div>;
});
