import { FileIcon, ImagePlusIcon, SlashIcon, SlidersHorizontalIcon, XIcon } from "lucide-react";
import { type ReactNode, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { client } from "~/api/client";
import type { ConfigOption, ContentBlock, SlashCommand, ThreadUsage } from "~/api/types";
import { useT } from "~/i18n";
import { formatTokens } from "~/lib/format";
import { cn } from "~/lib/utils";
import { actions, useStore } from "~/state/store";
import { errorMessage, toast } from "~/state/toast";
import { Popover, PopoverPopup, PopoverTrigger, Tip } from "../ui/overlay";
import { Spinner } from "../ui/primitives";
import { OptionPicker, composerControlClass } from "./OptionPicker";

interface Attachment {
  id: number;
  mimeType: string;
  data: string;
  url: string;
}

type Trigger = { kind: "mention" | "command"; query: string; start: number; end: number };

function findTrigger(text: string, caret: number): Trigger | null {
  const before = text.slice(0, caret);
  const command = /^\/(\S*)$/.exec(before);
  if (command) return { kind: "command", query: command[1]!, start: 0, end: caret };
  const mention = /(^|\s)@([^\s@]*)$/.exec(before);
  if (mention) return { kind: "mention", query: mention[2]!, start: caret - mention[2]!.length - 1, end: caret };
  return null;
}

function isImageFile(file: File) {
  return file.type.startsWith("image/") || (file.type === "" && /\.(png|jpe?g|gif|webp|bmp|avif)$/i.test(file.name));
}

function readImage(file: File): Promise<Omit<Attachment, "id">> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error);
    reader.onload = () => {
      const result = String(reader.result);
      const inferred = /^data:(image\/[\w.+-]+)/.exec(result)?.[1];
      resolve({ mimeType: file.type || inferred || "image/png", data: result.slice(result.indexOf(",") + 1), url: result });
    };
    reader.readAsDataURL(file);
  });
}

let attachmentSeq = 0;

/** The bar shows one model, one thinking level and one mode picker; the rest go behind a button. */
function partitionOptions(options: ConfigOption[]) {
  const modes = options.filter((option) => option.category === "mode");
  const picks = [
    options.find((option) => option.category === "model"),
    options.find((option) => option.category === "thought_level"),
    modes.find((option) => /permission/i.test(option.id)) ?? modes[0],
  ];
  const chosen = new Set(picks.filter((option) => option !== undefined).map((option) => option.id));
  return {
    primary: options.filter((option) => chosen.has(option.id)),
    extra: options.filter((option) => !chosen.has(option.id)),
  };
}

export interface ComposerProps {
  draftKey: string;
  agentName: string;
  cwd: string | null;
  options: ConfigOption[];
  commands: SlashCommand[];
  onOptionChange: (optionId: string, value: string) => void;
  onSubmit: (content: ContentBlock[]) => Promise<void>;
  onStop?: () => void;
  running?: boolean;
  midTurnInput?: boolean;
  imageInput: boolean;
  readOnly?: boolean;
  sendBlocked?: string | null;
  placeholder: string;
  leading?: ReactNode;
  below?: ReactNode;
  usage?: ThreadUsage | null;
  autoFocus?: boolean;
}

export function Composer(props: ComposerProps) {
  const t = useT();
  const draft = useStore((s) => s.drafts[props.draftKey] ?? "");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [mentioned, setMentioned] = useState<string[]>([]);
  const [trigger, setTrigger] = useState<Trigger | null>(null);
  const [highlight, setHighlight] = useState(0);
  const [files, setFiles] = useState<string[]>([]);
  const [sending, setSending] = useState(false);
  const [dragging, setDragging] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const setDraft = (text: string) => actions.setDraft(props.draftKey, text);

  useLayoutEffect(() => {
    const area = textareaRef.current;
    if (!area) return;
    area.style.height = "0px";
    area.style.height = `${Math.min(area.scrollHeight, Math.round(window.innerHeight * 0.4))}px`;
  }, [draft]);

  useEffect(() => {
    const idle = !document.activeElement || document.activeElement === document.body;
    if (props.autoFocus && idle && matchMedia("(pointer: fine)").matches) textareaRef.current?.focus();
  }, [props.autoFocus, props.draftKey]);

  useEffect(() => {
    if (trigger?.kind !== "mention" || !props.cwd) return;
    const cwd = props.cwd;
    const handle = setTimeout(() => {
      client()
        .request("fs.find", { cwd, query: trigger.query, limit: 12 })
        .then((result) => setFiles(result.paths))
        .catch(() => setFiles([]));
    }, 80);
    return () => clearTimeout(handle);
  }, [trigger?.kind, trigger?.query, props.cwd]);

  const suggestions = useMemo(() => {
    if (!trigger) return [];
    if (trigger.kind === "command") {
      const query = trigger.query.toLowerCase();
      return props.commands
        .filter((command) => command.name.toLowerCase().includes(query))
        .map((command) => ({ key: command.name, label: `/${command.name}`, detail: command.description ?? command.inputHint ?? "" }));
    }
    return files.map((path) => ({ key: path, label: path.split("/").at(-1) ?? path, detail: path }));
  }, [trigger, props.commands, files]);

  const updateTrigger = (text: string, caret: number) => {
    const next = findTrigger(text, caret);
    setTrigger(next);
    if (next?.kind !== trigger?.kind || next?.query !== trigger?.query) setHighlight(0);
  };

  const accept = (index: number) => {
    const choice = suggestions[index];
    if (!trigger || !choice) return;
    const insert = trigger.kind === "command" ? `/${choice.key} ` : `@${choice.key} `;
    const next = draft.slice(0, trigger.start) + insert + draft.slice(trigger.end);
    setDraft(next);
    if (trigger.kind === "mention") setMentioned((list) => (list.includes(choice.key) ? list : [...list, choice.key]));
    setTrigger(null);
    const caret = trigger.start + insert.length;
    requestAnimationFrame(() => {
      textareaRef.current?.focus();
      textareaRef.current?.setSelectionRange(caret, caret);
    });
  };

  const addFiles = async (list: FileList | File[]) => {
    const images = [...list].filter(isImageFile);
    if (images.length === 0) return;
    if (!props.imageInput) {
      toast.info(t("composer.noImages", { agent: props.agentName }));
      return;
    }
    try {
      const read = await Promise.all(images.map(readImage));
      setAttachments((current) => [...current, ...read.map((image) => ({ ...image, id: ++attachmentSeq }))]);
    } catch {
      toast.error(t("composer.attachFailed"));
    }
  };

  const hasContent = draft.trim().length > 0 || attachments.length > 0;
  const canSend = hasContent && !props.readOnly && !props.sendBlocked && !sending;

  const submit = async () => {
    if (!canSend) return;
    const text = draft;
    const content: ContentBlock[] = [];
    if (text.trim()) content.push({ type: "text", text });
    for (const image of attachments) content.push({ type: "image", mimeType: image.mimeType, data: image.data, uri: null });
    for (const path of mentioned) if (text.includes(`@${path}`)) content.push({ type: "file", path, name: path.split("/").at(-1) ?? null });
    setSending(true);
    try {
      await props.onSubmit(content);
      setDraft("");
      setAttachments([]);
      setMentioned([]);
    } catch (error) {
      toast.error(errorMessage(error));
    } finally {
      setSending(false);
    }
  };

  const { primary, extra } = useMemo(() => partitionOptions(props.options), [props.options]);
  const showStop = props.running && !hasContent && props.onStop;
  const contextUsed = props.usage?.lastTurn?.contextUsed ?? props.usage?.lastTurn?.input ?? null;
  const contextWindow = props.usage?.lastTurn?.contextWindow ?? props.usage?.total?.contextWindow ?? null;

  return (
    <div className="mx-auto w-full max-w-(--chat-content-max-width)">
      <div
        className={cn(
          "relative isolate w-full rounded-2xl border border-black/8 bg-card/90 shadow-composer backdrop-blur-[var(--glass-blur)] transition-colors dark:border-white/[0.06] dark:bg-surface-raised/90 dark:shadow-[0_14px_32px_-18px_rgb(0_0_0/75%)]",
          dragging && "border-primary/60 ring-2 ring-primary/20",
        )}
        onDragOver={(event) => {
          if ([...event.dataTransfer.items].some((item) => item.kind === "file")) {
            event.preventDefault();
            setDragging(true);
          }
        }}
        onDragLeave={() => setDragging(false)}
        onDrop={(event) => {
          event.preventDefault();
          setDragging(false);
          void addFiles(event.dataTransfer.files);
        }}
      >
        {trigger && (suggestions.length > 0 || trigger.kind === "mention") ? (
          <div className="dropdown-glass absolute inset-x-0 bottom-full z-20 mb-2 overflow-hidden rounded-xl shadow-[0_16px_40px_-18px_rgb(0_0_0/55%)]">
            <div className="px-3 pt-2 pb-1 text-xs font-medium text-muted-foreground">
              {trigger.kind === "command" ? t("composer.commands") : t("composer.files")}
            </div>
            <div className="max-h-64 overflow-y-auto p-1" role="listbox">
              {suggestions.length === 0 ? (
                <div className="px-2 py-1.5 text-sm text-muted-foreground">
                  {trigger.kind === "command" ? t("composer.noCommands") : t("composer.noFiles")}
                </div>
              ) : (
                suggestions.map((suggestion, index) => (
                  <button
                    type="button"
                    role="option"
                    aria-selected={index === highlight}
                    key={suggestion.key}
                    onMouseDown={(event) => event.preventDefault()}
                    onMouseEnter={() => setHighlight(index)}
                    onClick={() => accept(index)}
                    className={cn("flex w-full min-w-0 items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm", index === highlight && "bg-accent")}
                  >
                    {trigger.kind === "command" ? <SlashIcon className="size-3.5 shrink-0 text-muted-foreground" /> : <FileIcon className="size-3.5 shrink-0 text-muted-foreground" />}
                    <span className={cn("shrink-0 font-medium", trigger.kind === "command" && "font-mono text-[13px]")}>{suggestion.label}</span>
                    <span className="min-w-0 truncate text-xs text-muted-foreground">{suggestion.detail}</span>
                  </button>
                ))
              )}
            </div>
          </div>
        ) : null}
        {attachments.length > 0 ? (
          <div className="flex flex-wrap gap-2 px-3 pt-3">
            {attachments.map((attachment) => (
              <div key={attachment.id} className="group/attachment relative">
                <img src={attachment.url} alt="" className="size-14 rounded-lg border object-cover" />
                <button
                  type="button"
                  aria-label={t("composer.removeAttachment")}
                  onClick={() => setAttachments((list) => list.filter((a) => a.id !== attachment.id))}
                  className="absolute -top-1.5 -right-1.5 flex size-5 items-center justify-center rounded-full border bg-popover text-muted-foreground shadow-sm hover:text-foreground"
                >
                  <XIcon className="size-3" />
                </button>
              </div>
            ))}
          </div>
        ) : null}
        <textarea
          ref={textareaRef}
          value={draft}
          rows={1}
          disabled={props.readOnly}
          placeholder={props.placeholder}
          onChange={(event) => {
            setDraft(event.target.value);
            updateTrigger(event.target.value, event.target.selectionStart);
          }}
          onSelect={(event) => updateTrigger(event.currentTarget.value, event.currentTarget.selectionStart)}
          onBlur={() => setTimeout(() => setTrigger(null), 120)}
          onPaste={(event) => {
            const data = event.clipboardData;
            const fromFiles = [...data.files].filter(isImageFile);
            const images =
              fromFiles.length > 0
                ? fromFiles
                : [...data.items]
                    .map((item) => (item.kind === "file" && item.type.startsWith("image/") ? item.getAsFile() : null))
                    .filter((file): file is File => file !== null);
            if (images.length > 0) {
              event.preventDefault();
              void addFiles(images);
            }
          }}
          onKeyDown={(event) => {
            if (event.nativeEvent.isComposing || event.keyCode === 229) return;
            if (trigger && suggestions.length > 0) {
              if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                event.preventDefault();
                const delta = event.key === "ArrowDown" ? 1 : -1;
                setHighlight((value) => (value + delta + suggestions.length) % suggestions.length);
                return;
              }
              if (event.key === "Enter" || event.key === "Tab") {
                event.preventDefault();
                accept(highlight);
                return;
              }
            }
            if (event.key === "Escape" && trigger) {
              event.preventDefault();
              setTrigger(null);
              return;
            }
            if (event.key === "Enter" && !event.shiftKey) {
              event.preventDefault();
              void submit();
            }
          }}
          className="block max-h-[40vh] min-h-[3.25rem] w-full resize-none bg-transparent px-4 pt-3.5 pb-1 text-base leading-relaxed text-foreground outline-none placeholder:text-placeholder disabled:cursor-not-allowed sm:text-sm"
        />
        <div className="flex items-center gap-1 px-2 pt-1 pb-2">
          <div className="flex min-w-0 flex-1 items-center gap-0.5 overflow-x-auto [scrollbar-width:none]">
            {props.leading}
            <Tip label={props.imageInput ? t("composer.attach") : t("composer.noImages", { agent: props.agentName })}>
              <button
                type="button"
                className={cn(composerControlClass, "px-1.5")}
                aria-label={t("composer.attach")}
                disabled={!props.imageInput || props.readOnly}
                onClick={() => fileInputRef.current?.click()}
              >
                <ImagePlusIcon />
              </button>
            </Tip>
            {primary.map((option) => (
              <OptionPicker
                key={option.id}
                option={option}
                compact={option.category !== "model"}
                disabled={props.readOnly}
                onChange={(value) => props.onOptionChange(option.id, value)}
              />
            ))}
            {extra.length > 0 ? (
              <Popover>
                <Tip label={t("composer.moreOptions")}>
                  <PopoverTrigger disabled={props.readOnly} className={cn(composerControlClass, "px-1.5")} aria-label={t("composer.moreOptions")}>
                    <SlidersHorizontalIcon />
                  </PopoverTrigger>
                </Tip>
                <PopoverPopup side="top" align="start" className="w-80 p-1.5">
                  {extra.map((option) => (
                    <div key={option.id} className="flex min-w-0 items-center justify-between gap-3 rounded-md ps-2">
                      <span className="min-w-0 truncate text-sm text-muted-foreground" title={option.description ?? option.name}>
                        {option.name}
                      </span>
                      <OptionPicker option={option} disabled={props.readOnly} onChange={(value) => props.onOptionChange(option.id, value)} />
                    </div>
                  ))}
                </PopoverPopup>
              </Popover>
            ) : null}
          </div>
          <input
            ref={fileInputRef}
            type="file"
            accept="image/*"
            multiple
            hidden
            onChange={(event) => {
              if (event.target.files) void addFiles(event.target.files);
              event.target.value = "";
            }}
          />
          {contextUsed && contextWindow ? <ContextMeter used={contextUsed} window={contextWindow} usage={props.usage ?? null} /> : null}
          {showStop ? (
            <Tip label={t("composer.stop")}>
              <button
                type="button"
                aria-label={t("composer.stop")}
                onClick={props.onStop}
                className="flex size-8 shrink-0 cursor-pointer items-center justify-center rounded-full bg-destructive/90 text-white shadow-xs shadow-destructive/24 inset-shadow-[0_1px_rgb(255_255_255/16%)] transition-colors hover:bg-destructive"
              >
                <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor" aria-hidden="true">
                  <rect x="2" y="2" width="8" height="8" rx="1.5" />
                </svg>
              </button>
            </Tip>
          ) : (
            <Tip label={props.sendBlocked ?? (props.running ? (props.midTurnInput ? t("composer.steer") : t("composer.queue")) : t("composer.send"))}>
              <button
                type="button"
                aria-label={t("composer.send")}
                disabled={!canSend}
                onClick={() => void submit()}
                className="relative flex size-9 shrink-0 items-center justify-center rounded-full bg-message-action text-message-action-foreground shadow-xs shadow-message-action/24 transition-all duration-150 enabled:cursor-pointer enabled:inset-shadow-[0_1px_rgb(255_255_255/16%)] enabled:hover:scale-105 enabled:hover:bg-message-action-hover disabled:bg-foreground/12 disabled:text-muted-foreground disabled:shadow-none sm:size-8"
              >
                {sending ? (
                  <Spinner className="size-3.5" />
                ) : (
                  <svg width="14" height="14" viewBox="0 0 14 14" fill="none" aria-hidden="true">
                    <path d="M7 11.5V2.5M7 2.5L3 6.5M7 2.5L11 6.5" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" />
                  </svg>
                )}
              </button>
            </Tip>
          )}
        </div>
      </div>
      {props.below}
    </div>
  );
}

function ContextMeter({ used, window, usage }: { used: number; window: number; usage: ThreadUsage | null }) {
  const t = useT();
  const ratio = Math.min(1, used / window);
  const radius = 7;
  const circumference = 2 * Math.PI * radius;
  const label = (
    <div className="flex flex-col gap-0.5">
      <span>{t("composer.context", { used: formatTokens(used), window: formatTokens(window) })}</span>
      {usage?.rateLimits.map((limit) => (
        <span key={limit.label} className="text-muted-foreground">
          {limit.label}: {Math.round(limit.usedPercent)}%
        </span>
      ))}
    </div>
  );
  return (
    <Tip label={label}>
      <span className="me-1 hidden h-7 shrink-0 items-center gap-1.5 rounded-md px-1.5 text-xs tabular-nums text-muted-foreground sm:inline-flex">
        <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden className="-rotate-90">
          <circle cx="8" cy="8" r={radius} fill="none" strokeWidth="2" className="stroke-foreground/12" />
          <circle
            cx="8"
            cy="8"
            r={radius}
            fill="none"
            strokeWidth="2"
            strokeLinecap="round"
            strokeDasharray={circumference}
            strokeDashoffset={circumference * (1 - ratio)}
            className={ratio > 0.85 ? "stroke-warning" : "stroke-muted-foreground"}
          />
        </svg>
        {Math.round(ratio * 100)}%
      </span>
    </Tip>
  );
}
