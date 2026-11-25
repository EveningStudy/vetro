import { CheckIcon, CodeIcon, CopyIcon, EyeIcon, FolderOpenIcon } from "lucide-react";
import { type Token, type Tokens, Lexer } from "marked";
import { Fragment, memo, type ReactNode, useMemo, useState } from "react";
import { useT } from "~/i18n";
import { cn, copyText } from "~/lib/utils";
import { actions, useIsLocalDesktop, useStore } from "~/state/store";
import { toast } from "~/state/toast";
import { Menu, MenuItem, MenuPopup, MenuSeparator, MenuTrigger } from "../ui/overlay";
import { useHighlighter } from "./highlight";
import { useThreadCwd } from "./threadContext";

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

interface FileTarget {
  path: string;
  line?: number;
}

const WINDOWS_ABSOLUTE = /^[A-Za-z]:[\\/]/;
const UNC_PATH = /^\\\\[^\\/\s]+[\\/]/;
const HOME_PATH = /^~[\\/]./;
const LINE_SUFFIX = /(?:#L(\d+)(?:C\d+)?(?:-L?\d+(?:C\d+)?)?|:(\d+)(?::\d+)?(?:-\d+)?)$/;
const UNIX_ROOTS = new Set(["Users", "home", "usr", "opt", "etc", "var", "tmp", "private", "Volumes", "mnt", "root", "srv", "Applications", "Library", "nix", "bin", "sbin"]);
const DOMAIN_LIKE = /^(?:www\.|[\w-]+\.(?:com|org|net|io|dev|ai|app|co|sh)$)/i;
const RELATIVE_SEGMENT = /^[\w@.+\-()[\]]+$/;
const NO_PREVIEW = /\.(?:exe|msi|dll|dmg|appimage|so|dylib|pkg|deb|rpm|iso|zip|7z|gz|tar|rar)$/i;

function splitLine(value: string): FileTarget {
  const match = LINE_SUFFIX.exec(value);
  if (!match || match.index === 0) return { path: value };
  const line = Number(match[1] ?? match[2]);
  const path = value.slice(0, match.index);
  return line > 0 ? { path, line } : { path };
}

function hasExtension(name: string) {
  const ext = /\.([A-Za-z0-9]{1,12})$/.exec(name);
  return !!ext && /[A-Za-z]/.test(ext[1]!);
}

function isAbsolutePath(path: string) {
  return WINDOWS_ABSOLUTE.test(path) || UNC_PATH.test(path) || HOME_PATH.test(path) || path.startsWith("/");
}

function safeDecode(value: string) {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

/** A workspace-relative path that is clearly a file: separators, plain segments, a real extension. */
function isRelativeFile(path: string) {
  if (!/[\\/]/.test(path) || /^[@\-~]/.test(path)) return false;
  const segments = path.replace(/^\.{1,2}[\\/]/, "").split(/[\\/]/);
  if (segments.some((segment) => !RELATIVE_SEGMENT.test(segment))) return false;
  return hasExtension(segments.at(-1)!) && !DOMAIN_LIKE.test(segments[0]!);
}

/** An inline code span that names a local file or folder, with an optional `:line[:col]`. */
function codePath(text: string, hasCwd: boolean): FileTarget | null {
  const raw = text.trim();
  if (!raw || raw.length > 400 || /[\n<>"|*?`$]/.test(raw) || /^[a-z][a-z0-9+.-]*:\/\//i.test(raw)) return null;
  const target = splitLine(raw);
  const path = target.path;
  if (WINDOWS_ABSOLUTE.test(path) || UNC_PATH.test(path)) return path.length > 3 && !/\s-/.test(path) ? target : null;
  if (/\s/.test(path)) return null;
  if (HOME_PATH.test(path)) return target;
  if (path.startsWith("/")) {
    const segments = path.split("/").filter(Boolean);
    if (segments.length < 2) return null;
    return hasExtension(segments.at(-1)!) || UNIX_ROOTS.has(segments[0]!) ? target : null;
  }
  return hasCwd && isRelativeFile(path) ? target : null;
}

/** A Markdown link target that points at a local file: `file://` URLs or scheme-less paths. */
function linkPath(href: string, hasCwd: boolean): FileTarget | null {
  const raw = href.trim();
  if (!raw || raw.startsWith("#") || raw.startsWith("//")) return null;
  if (/^file:/i.test(raw)) {
    let rest = raw.slice(5);
    let fragment = "";
    const hash = rest.indexOf("#");
    if (hash >= 0) {
      fragment = rest.slice(hash);
      rest = rest.slice(0, hash);
    }
    rest = rest.replace(/\?.*$/, "");
    if (rest.startsWith("//")) {
      rest = rest.slice(2);
      const slash = rest.indexOf("/");
      const host = slash < 0 ? rest : rest.slice(0, slash);
      const tail = slash < 0 ? "" : rest.slice(slash);
      rest = !host || host.toLowerCase() === "localhost" ? tail : `//${host}${tail}`;
    }
    let path = safeDecode(rest);
    if (/^\/[A-Za-z]:(?:[\\/]|$)/.test(path)) path = path.slice(1).replace(/\//g, "\\");
    else if (path.startsWith("//")) path = path.replace(/\//g, "\\");
    if (!path || path === "/") return null;
    const target = splitLine(/^#L\d/.test(fragment) ? path + fragment : path);
    return target.path ? target : null;
  }
  if (/^[a-z][a-z0-9+.-]*:/i.test(raw) && !WINDOWS_ABSOLUTE.test(raw)) return null;
  const target = splitLine(safeDecode(raw));
  if (isAbsolutePath(target.path)) return target.path.length > 1 ? target : null;
  if (!hasCwd) return null;
  const segments = target.path.replace(/^\.{1,2}[\\/]/, "").split(/[\\/]/);
  const last = segments.at(-1) ?? "";
  return hasExtension(last) || (segments.length > 1 && segments.slice(0, last ? undefined : -1).every((s) => RELATIVE_SEGMENT.test(s)))
    ? target
    : null;
}

function resolveHome(path: string, home: string | undefined) {
  if (!home || !/^~(?=[\\/]|$)/.test(path)) return path;
  const separator = home.includes("\\") ? "\\" : "/";
  return home.replace(/[\\/]+$/, "") + path.slice(1).replace(/[\\/]/g, separator);
}

function FileRef({ target, variant, children }: { target: FileTarget; variant: "code" | "link"; children: ReactNode }) {
  const t = useT();
  const local = useIsLocalDesktop();
  const cwd = useThreadCwd();
  const home = useStore((s) => s.app?.homeDir);
  const label = target.line ? `${target.path}:${target.line}` : target.path;
  const className = cn("chat-file-ref", variant === "link" && "chat-file-ref-link");
  const copy = () => void copyText(target.path).then(() => toast.success(t("common.copied")));
  const previewable = !NO_PREVIEW.test(target.path);
  if (!local) {
    const preview = !!cwd && previewable && !isAbsolutePath(target.path);
    return (
      <button
        type="button"
        title={label}
        className={className}
        onClick={preview && cwd ? () => actions.openPreviewDrawer(cwd, target.path, target.line) : copy}
      >
        {children}
      </button>
    );
  }
  const path = resolveHome(target.path, home);
  const openCwd = isAbsolutePath(target.path) ? "" : (cwd ?? "");
  return (
    <Menu>
      <MenuTrigger title={label} className={className}>
        {children}
      </MenuTrigger>
      <MenuPopup className="max-w-[min(24rem,calc(100vw-2rem))]">
        <div title={label} dir="rtl" className="truncate px-2 pt-1 pb-1.5 text-left font-mono text-2xs text-muted-foreground">
          <bdi>{label}</bdi>
        </div>
        {previewable ? (
          <MenuItem onClick={() => actions.openPreviewDrawer(openCwd, path, target.line)}>
            <EyeIcon />
            {t("open.preview")}
          </MenuItem>
        ) : null}
        <MenuItem onClick={() => void actions.openExternal(openCwd, "folder", path)}>
          <FolderOpenIcon />
          {t("open.reveal")}
        </MenuItem>
        {previewable ? (
          <MenuItem onClick={() => void actions.openExternal(openCwd, "editor", path, target.line)}>
            <CodeIcon />
            {t("open.editor")}
          </MenuItem>
        ) : null}
        <MenuSeparator />
        <MenuItem onClick={copy}>
          <CopyIcon />
          {t("open.copyFilePath")}
        </MenuItem>
      </MenuPopup>
    </Menu>
  );
}

function InlineCode({ text }: { text: string }) {
  const cwd = useThreadCwd();
  const target = useMemo(() => codePath(text, !!cwd), [text, cwd]);
  return target ? (
    <FileRef target={target} variant="code">
      <code>{text}</code>
    </FileRef>
  ) : (
    <code>{text}</code>
  );
}

function LocalLink({ href, text, children }: { href: string; text: string; children: ReactNode }) {
  const cwd = useThreadCwd();
  const target = useMemo(() => linkPath(href, !!cwd), [href, cwd]);
  return target ? (
    <FileRef target={target} variant="link">
      {children}
    </FileRef>
  ) : (
    <code>{text}</code>
  );
}

function inline(tokens: Token[] | undefined, keyPrefix = "", plain = false): ReactNode {
  if (!tokens) return null;
  return tokens.map((token, index) => {
    const key = `${keyPrefix}${index}`;
    switch (token.type) {
      case "text":
        return "tokens" in token && token.tokens ? <Fragment key={key}>{inline(token.tokens, `${key}.`, plain)}</Fragment> : <Fragment key={key}>{decode(token.text)}</Fragment>;
      case "escape":
        return <Fragment key={key}>{decode(token.text)}</Fragment>;
      case "strong":
        return <strong key={key}>{inline((token as Tokens.Strong).tokens, `${key}.`, plain)}</strong>;
      case "em":
        return <em key={key}>{inline((token as Tokens.Em).tokens, `${key}.`, plain)}</em>;
      case "del":
        return <del key={key}>{inline((token as Tokens.Del).tokens, `${key}.`, plain)}</del>;
      case "codespan":
        return plain ? <code key={key}>{decode(token.text)}</code> : <InlineCode key={key} text={decode(token.text)} />;
      case "br":
        return <br key={key} />;
      case "link": {
        const link = token as Tokens.Link;
        const href = safeHref(link.href);
        if (href) {
          return (
            <a key={key} href={href} target="_blank" rel="noreferrer noopener">
              {inline(link.tokens, `${key}.`, true)}
            </a>
          );
        }
        if (plain) return <code key={key}>{link.text}</code>;
        return (
          <LocalLink key={key} href={link.href} text={link.text}>
            {inline(link.tokens, `${key}.`, true)}
          </LocalLink>
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
