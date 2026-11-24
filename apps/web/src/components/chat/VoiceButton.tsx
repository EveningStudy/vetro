import { DownloadIcon, MicIcon, SettingsIcon, SquareIcon, XIcon } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { client } from "~/api/client";
import { useT } from "~/i18n";
import { type RecordingSession, startRecording } from "~/lib/audioRecorder";
import { cn, isMac } from "~/lib/utils";
import { actions, useIsLocalDesktop, useStore } from "~/state/store";
import { errorMessage, toast } from "~/state/toast";
import { Button } from "../ui/button";
import { Popover, PopoverPopup, PopoverTrigger, Tip } from "../ui/overlay";
import { Spinner } from "../ui/primitives";
import { composerControlClass } from "./OptionPicker";

function formatMb(bytes: number) {
  return `${Math.max(1, Math.round(bytes / (1024 * 1024)))} MB`;
}

function formatClock(seconds: number) {
  const mins = Math.floor(seconds / 60);
  const secs = seconds % 60;
  return `${mins}:${String(secs).padStart(2, "0")}`;
}

export function VoiceButton({
  disabled,
  onTranscript,
}: {
  disabled?: boolean;
  onTranscript: (text: string) => void;
}) {
  const t = useT();
  const voice = useStore((s) => s.voice);
  const progress = useStore((s) => s.voiceProgress);
  const localDesktop = useIsLocalDesktop();
  const [phase, setPhase] = useState<"idle" | "recording" | "transcribing">("idle");
  const [elapsed, setElapsed] = useState(0);
  const [level, setLevel] = useState(0);
  const [setupOpen, setSetupOpen] = useState(false);
  const recorderRef = useRef<RecordingSession | null>(null);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const stopTimer = useCallback(() => {
    if (timerRef.current) {
      clearInterval(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const cancel = useCallback(() => {
    stopTimer();
    recorderRef.current?.cancel();
    recorderRef.current = null;
    setPhase("idle");
    setElapsed(0);
    setLevel(0);
  }, [stopTimer]);

  const stopAndTranscribe = useCallback(async () => {
    const active = recorderRef.current;
    if (!active) return;
    recorderRef.current = null;
    stopTimer();
    setPhase("transcribing");
    setLevel(0);
    try {
      const { base64, durationMs } = await active.stop();
      if (durationMs < 250) {
        setPhase("idle");
        return;
      }
      const result = await client().request("voice.transcribe", { audioBase64: base64 });
      const text = result.text.trim();
      if (text) onTranscript(text);
      else toast.info(t("voice.empty"));
    } catch (error) {
      toast.error(errorMessage(error));
    } finally {
      setPhase("idle");
      setElapsed(0);
    }
  }, [onTranscript, stopTimer, t]);

  const begin = useCallback(async () => {
    if (disabled || phase !== "idle") return;
    try {
      const session = await startRecording(setLevel);
      recorderRef.current = session;
      setElapsed(0);
      setPhase("recording");
      const started = Date.now();
      timerRef.current = setInterval(() => {
        setElapsed(Math.floor((Date.now() - started) / 1000));
      }, 250);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t("voice.micDenied"));
    }
  }, [disabled, phase, t]);

  useEffect(() => cancel, [cancel]);

  useEffect(() => {
    if (phase !== "recording") return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        cancel();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [phase, cancel]);

  const installing = !!voice?.installing || (!!progress && !progress.done);
  const ready = !!voice?.ready;
  const percent = progress && progress.totalBytes > 0 ? Math.min(100, Math.round((progress.downloadedBytes / progress.totalBytes) * 100)) : 0;

  if (phase === "recording") {
    return (
      <div className="inline-flex h-7 shrink-0 items-center gap-1 rounded-full border border-destructive/35 bg-destructive/10 ps-2 pe-1 text-xs tabular-nums text-destructive-foreground">
        <span
          className="size-2 shrink-0 rounded-full bg-destructive transition-transform"
          style={{ transform: `scale(${0.85 + level * 0.55})` }}
        />
        <span className="min-w-7 text-center">{formatClock(elapsed)}</span>
        <Tip label={t("voice.finish")}>
          <button
            type="button"
            aria-label={t("voice.finish")}
            onClick={() => void stopAndTranscribe()}
            className="inline-flex size-5 cursor-pointer items-center justify-center rounded-full bg-destructive text-white hover:bg-destructive/90"
          >
            <SquareIcon className="size-2.5 fill-current" />
          </button>
        </Tip>
        <Tip label={t("common.cancel")}>
          <button
            type="button"
            aria-label={t("common.cancel")}
            onClick={cancel}
            className="inline-flex size-5 cursor-pointer items-center justify-center rounded-full text-muted-foreground hover:bg-accent hover:text-foreground"
          >
            <XIcon className="size-3" />
          </button>
        </Tip>
      </div>
    );
  }

  if (phase === "transcribing") {
    return (
      <span className="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-md px-2 text-xs text-muted-foreground">
        <Spinner className="size-3.5" />
        <span className="max-sm:hidden">{t("voice.transcribing")}</span>
      </span>
    );
  }

  if (ready) {
    return (
      <Tip label={t("voice.dictate")}>
        <button
          type="button"
          disabled={disabled}
          aria-label={t("voice.dictate")}
          onClick={() => void begin()}
          className={cn(composerControlClass, "px-1.5")}
        >
          <MicIcon />
        </button>
      </Tip>
    );
  }

  return (
    <Popover
      open={setupOpen}
      onOpenChange={(next) => {
        setSetupOpen(next);
        if (next) void actions.refreshVoice();
      }}
    >
      <Tip label={installing ? `${t("voice.installing")} ${percent}%` : t("voice.dictate")}>
        <PopoverTrigger disabled={disabled} className={cn(composerControlClass, "px-1.5")} aria-label={t("voice.dictate")}>
          {installing ? <Spinner className="size-3.5" /> : <MicIcon />}
        </PopoverTrigger>
      </Tip>
      <PopoverPopup side="top" align="end" className="w-72 p-3">
        <div className="flex flex-col gap-2.5">
          <div className="flex items-center justify-between gap-2">
            <span className="text-sm font-medium text-foreground">{t("voice.title")}</span>
            {voice?.downloadBytes ? (
              <span className="text-xs tabular-nums text-muted-foreground">{formatMb(voice.downloadBytes)}</span>
            ) : null}
          </div>
          <div className="text-xs text-muted-foreground">{voice?.modelName ?? "SenseVoice-Small (int8)"}</div>
          {installing ? (
            <div className="flex flex-col gap-1.5">
              <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
                <div className="h-full bg-primary transition-[width]" style={{ width: `${percent}%` }} />
              </div>
              <div className="flex items-center justify-between text-[11px] tabular-nums text-muted-foreground">
                <span>{t(progress?.stage.startsWith("extracting") ? "voice.extracting" : "voice.downloading")}</span>
                <span>{percent}%</span>
              </div>
            </div>
          ) : (
            <div className="flex items-center gap-2">
              {localDesktop && voice?.supported !== false ? (
                <Button
                  size="sm"
                  className="flex-1"
                  onClick={() => {
                    void actions.installVoice().then((ok) => {
                      if (ok) setSetupOpen(false);
                    });
                  }}
                >
                  <DownloadIcon />
                  {t("voice.download")}
                </Button>
              ) : null}
              <Button
                size="sm"
                variant="outline"
                onClick={() => {
                  setSetupOpen(false);
                  actions.navigate({ view: "settings", section: "voice" });
                }}
              >
                <SettingsIcon />
                {t("sidebar.settings")}
              </Button>
            </div>
          )}
          <div className="text-[11px] text-muted-foreground/80">
            {t("voice.osTip", { key: isMac ? "Fn Fn" : "Win + H" })}
          </div>
        </div>
      </PopoverPopup>
    </Popover>
  );
}
