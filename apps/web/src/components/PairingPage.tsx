import { useRef, useState } from "react";
import { authApi } from "~/api/client";
import { newPairingSession } from "~/api/session";
import { useT } from "~/i18n";
import { Button } from "./ui/button";
import { Input, Label, Spinner } from "./ui/primitives";
import { VerreMark } from "./VerreMark";

function defaultDeviceName() {
  const ua = navigator.userAgent;
  if (/iPhone/.test(ua)) return "iPhone";
  if (/iPad/.test(ua)) return "iPad";
  if (/Android/.test(ua)) return "Android";
  if (/Mac/.test(ua)) return "Mac";
  if (/Windows/.test(ua)) return "Windows PC";
  return "Browser";
}

export function takePairingCode() {
  const url = new URL(location.href);
  const fragment = new URLSearchParams(url.hash.slice(1));
  const code = fragment.get("pair") ?? url.searchParams.get("code") ?? "";
  if (fragment.has("pair") || url.searchParams.has("code")) {
    if (fragment.has("pair")) {
      fragment.delete("pair");
      url.hash = fragment.toString();
    }
    url.searchParams.delete("code");
    history.replaceState(null, "", url);
  }
  return code;
}

export function PairingPage({ initialCode = "", onPaired }: { initialCode?: string; onPaired: () => void }) {
  const t = useT();
  const [code, setCode] = useState(initialCode);
  const [name, setName] = useState(defaultDeviceName);
  const [remember, setRemember] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const proposedSession = useRef<string | null>(null);
  if (/^(127\.0\.0\.1|localhost|\[::1\])$/.test(location.hostname))
    return (
      <div className="flex min-h-dvh items-center justify-center bg-background px-4 py-10">
        <div className="flex w-full max-w-sm flex-col items-center gap-3 rounded-2xl border bg-card/80 p-6 text-center shadow-composer dark:bg-surface-raised/80">
          <VerreMark className="size-9" />
          <h1 className="text-lg font-semibold">{t("pair.localTitle")}</h1>
          <p className="text-sm text-muted-foreground">{t("pair.local")}</p>
        </div>
      </div>
    );
  return (
    <div className="flex min-h-dvh items-center justify-center bg-background px-4 py-10">
      <form
        className="flex w-full max-w-sm flex-col gap-5 rounded-2xl border bg-card/80 p-6 shadow-composer dark:bg-surface-raised/80"
        onSubmit={(event) => {
          event.preventDefault();
          if (busy) return;
          proposedSession.current ??= newPairingSession();
          setBusy(true);
          setError(null);
          authApi()
            .pair(code.trim(), name.trim() || defaultDeviceName(), proposedSession.current, remember)
            .then((result) => {
              if (result.ok) {
                onPaired();
              } else if (result.status === 401) setError(t("pair.invalid"));
              else if (result.status === 429) setError(t("pair.rateLimited"));
              else setError(result.message ?? t("pair.failed"));
            })
            .catch((error: unknown) => setError(
              error instanceof DOMException && error.name === "TimeoutError"
                ? t("pair.timeout")
                : t("pair.failed"),
            ))
            .finally(() => setBusy(false));
        }}
      >
        <div className="flex flex-col items-center gap-3 text-center">
          <VerreMark className="size-9" />
          <div>
            <h1 className="text-lg font-semibold">{t("pair.title")}</h1>
            <p className="mt-1 text-sm text-muted-foreground">{t("pair.body")}</p>
          </div>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="pair-code">{t("pair.code")}</Label>
          <Input
            id="pair-code"
            value={code}
            autoFocus
            autoComplete="one-time-code"
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
            disabled={busy}
            onChange={(event) => {
              proposedSession.current = null;
              setCode(event.target.value.trim());
            }}
            placeholder="503bb401"
            className="h-11 text-center font-mono text-lg tracking-[0.2em] sm:h-11 sm:text-lg"
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="pair-name">{t("pair.deviceName")}</Label>
          <Input id="pair-name" maxLength={100} value={name} disabled={busy} onChange={(event) => {
            proposedSession.current = null;
            setName(event.target.value);
          }} />
        </div>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={remember} disabled={busy} onChange={(event) => setRemember(event.target.checked)} />
          {t("pair.remember")}
        </label>
        {error ? <p className="text-sm text-destructive-foreground">{error}</p> : null}
        <Button type="submit" size="lg" disabled={busy || code.trim().length < 4}>
          {busy ? <Spinner className="size-4" /> : null}
          {t("pair.submit")}
        </Button>
      </form>
    </div>
  );
}
