import { CheckIcon, CopyIcon, ExternalLinkIcon, LogInIcon, RefreshCwIcon } from "lucide-react";
import { useEffect, useState } from "react";
import { client } from "~/api/client";
import type { AgentInfo, AuthenticateResult } from "~/api/types";
import { useT } from "~/i18n";
import { cn, copyText } from "~/lib/utils";
import { useStore } from "~/state/store";
import { errorMessage, toast } from "~/state/toast";
import { Button } from "./ui/button";
import { Spinner } from "./ui/primitives";

/** The agent's own sign-in methods. A device-code flow returns a link and a code that the user
 * finishes elsewhere; we show them and wait until the agent reports that it is signed in. */
export function SignInMethods({ agent, size = "sm", onDone }: { agent: AgentInfo; size?: "sm" | "xs"; onDone?: () => void }) {
  const t = useT();
  const [pending, setPending] = useState<string | null>(null);
  const [result, setResult] = useState<AuthenticateResult | null>(null);
  const [checking, setChecking] = useState(false);
  const [copied, setCopied] = useState(false);
  const signedIn = agent.auth.state === "ok";

  useEffect(() => {
    if (signedIn && result) {
      setResult(null);
      onDone?.();
    }
  }, [signedIn, result, onDone]);

  const refresh = () => {
    setChecking(true);
    return client()
      .request("agents.refresh", {})
      .then((response) => {
        useStore.setState({ agents: response.agents });
        return response.agents.find((a) => a.id === agent.id)?.auth.state === "ok";
      })
      .finally(() => setChecking(false));
  };

  const start = (methodId: string) => {
    setPending(methodId);
    setResult(null);
    client()
      .request("agents.authenticate", { agentId: agent.id, methodId })
      .then(async (response) => {
        if (response.url || response.code || response.message) setResult(response);
        const ok = await refresh();
        if (ok && !(response.url || response.code)) onDone?.();
      })
      .catch((error) => toast.error(errorMessage(error)))
      .finally(() => setPending(null));
  };

  return (
    <div className="flex min-w-0 flex-col gap-2.5">
      <div className="flex flex-wrap gap-2">
        {agent.auth.methods.map((method, index) => (
          <Button
            key={method.id}
            size={size}
            variant={index === 0 ? "default" : "outline"}
            disabled={pending !== null}
            title={method.description ?? undefined}
            onClick={() => start(method.id)}
          >
            {pending === method.id ? <Spinner className="size-3.5" /> : <LogInIcon />}
            {pending === method.id ? t("composer.signingIn") : method.name}
          </Button>
        ))}
      </div>
      {result ? (
        <div className="flex flex-col gap-2 rounded-lg border bg-background/70 px-3 py-2.5 text-sm">
          {result.message ? <p className="text-muted-foreground">{result.message}</p> : null}
          {result.code ? (
            <div className="flex items-center gap-2">
              <span className="font-mono text-lg font-semibold tracking-[0.15em] tabular-nums">{result.code}</span>
              <button
                type="button"
                aria-label={t("common.copy")}
                onClick={() => {
                  void copyText(result.code ?? "").then(() => {
                    setCopied(true);
                    setTimeout(() => setCopied(false), 1200);
                  });
                }}
                className="inline-flex size-7 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
              >
                {copied ? <CheckIcon className="size-3.5" /> : <CopyIcon className="size-3.5" />}
              </button>
            </div>
          ) : null}
          {result.url ? (
            <a href={result.url} target="_blank" rel="noreferrer noopener" className="inline-flex min-w-0 items-center gap-1.5 text-info-foreground hover:underline">
              <span className="truncate font-mono text-xs">{result.url}</span>
              <ExternalLinkIcon className="size-3.5 shrink-0" />
            </a>
          ) : null}
          <div className={cn("flex items-center gap-2 text-xs text-muted-foreground")}>
            <Spinner className="size-3" />
            <span className="flex-1">{t("signin.waiting", { agent: agent.name })}</span>
            <Button size="xs" variant="ghost-muted" disabled={checking} onClick={() => void refresh()}>
              <RefreshCwIcon />
              {t("signin.check")}
            </Button>
          </div>
        </div>
      ) : null}
    </div>
  );
}
