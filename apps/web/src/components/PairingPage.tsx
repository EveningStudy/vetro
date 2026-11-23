import { useState } from "react";
import { authApi } from "~/api/client";
import { useT } from "~/i18n";
import { Button } from "./ui/button";
import { Input, Label, Spinner } from "./ui/primitives";
import { VetroMark } from "./VetroMark";

function defaultDeviceName() {
  const ua = navigator.userAgent;
  if (/iPhone/.test(ua)) return "iPhone";
  if (/iPad/.test(ua)) return "iPad";
  if (/Android/.test(ua)) return "Android";
  if (/Mac/.test(ua)) return "Mac";
  if (/Windows/.test(ua)) return "Windows PC";
  return "Browser";
}

export function PairingPage({ onPaired }: { onPaired: () => void }) {
  const t = useT();
  const [code, setCode] = useState(() => new URLSearchParams(location.search).get("code") ?? "");
  const [name, setName] = useState(defaultDeviceName);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <div className="flex min-h-dvh items-center justify-center bg-background px-4 py-10">
      <form
        className="flex w-full max-w-sm flex-col gap-5 rounded-2xl border bg-card/80 p-6 shadow-composer dark:bg-surface-raised/80"
        onSubmit={(event) => {
          event.preventDefault();
          setBusy(true);
          setError(null);
          authApi()
            .pair(code.trim(), name.trim() || defaultDeviceName())
            .then((result) => {
              if (result.ok) {
                const url = new URL(location.href);
                url.searchParams.delete("code");
                history.replaceState(null, "", url);
                onPaired();
              } else setError(result.message ?? t("pair.failed"));
            })
            .catch((err: unknown) => setError(err instanceof Error ? err.message : t("pair.failed")))
            .finally(() => setBusy(false));
        }}
      >
        <div className="flex flex-col items-center gap-3 text-center">
          <VetroMark className="size-9" />
          <div>
            <h1 className="text-lg font-semibold">{t("pair.title")}</h1>
            <p className="mt-1 text-sm text-muted-foreground">{t("pair.body")}</p>
            {/^(127\.0\.0\.1|localhost|\[::1\])$/.test(location.hostname) ? (
              <p className="mt-2 text-xs text-muted-foreground">{t("pair.local")}</p>
            ) : null}
          </div>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="pair-code">{t("pair.code")}</Label>
          <Input
            id="pair-code"
            value={code}
            autoFocus
            autoComplete="one-time-code"
            autoCapitalize="characters"
            spellCheck={false}
            onChange={(event) => setCode(event.target.value.toUpperCase())}
            placeholder="ABC-123"
            className="h-11 text-center font-mono text-lg tracking-[0.2em] sm:h-11 sm:text-lg"
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="pair-name">{t("pair.deviceName")}</Label>
          <Input id="pair-name" value={name} onChange={(event) => setName(event.target.value)} />
        </div>
        {error ? <p className="text-sm text-destructive-foreground">{error}</p> : null}
        <Button type="submit" size="lg" disabled={busy || code.trim().length < 4}>
          {busy ? <Spinner className="size-4" /> : null}
          {t("pair.submit")}
        </Button>
      </form>
    </div>
  );
}
