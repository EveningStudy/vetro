import { useEffect, useRef, useState } from "react";
import { authApi } from "~/api/client";
import { htmlLangTag, useDisplayLang, useT } from "~/i18n";
import { PairingPage } from "./components/PairingPage";
import { Shell } from "./components/Shell";
import { TooltipProvider } from "./components/ui/overlay";
import { Spinner } from "./components/ui/primitives";
import { startSync, useStore } from "./state/store";

type AuthState = "checking" | "ok" | "pair" | "error";

export function App({ pairingCode, localToken }: { pairingCode: string; localToken: string | null }) {
  const t = useT();
  const displayLang = useDisplayLang();
  const [auth, setAuth] = useState<AuthState>("checking");
  const authLost = useStore((s) => s.authLost);
  const checking = useRef(false);
  const bootstrap = useRef(localToken);

  useEffect(() => {
    document.documentElement.lang = htmlLangTag(displayLang);
  }, [displayLang]);

  const check = async () => {
    if (checking.current) return;
    checking.current = true;
    setAuth("checking");
    try {
      const api = authApi();
      if (bootstrap.current) {
        await api.local(bootstrap.current);
        bootstrap.current = null;
      }
      setAuth(await api.session() ? "ok" : "pair");
    } catch {
      setAuth("error");
    } finally {
      checking.current = false;
    }
  };

  useEffect(() => { void check(); }, []);

  useEffect(() => {
    if (auth === "ok") startSync();
  }, [auth]);

  if (auth === "checking")
    return (
      <div className="flex h-dvh items-center justify-center">
        <Spinner className="text-muted-foreground" />
      </div>
    );
  if (auth === "pair") return <PairingPage initialCode={pairingCode} onPaired={() => setAuth("ok")} />;
  if (authLost) return <PairingPage onPaired={() => location.reload()} />;
  if (auth === "error")
    return (
      <div className="flex h-dvh flex-col items-center justify-center gap-3 text-sm text-muted-foreground">
        {t("conn.lost")}
        <button type="button" className="text-foreground underline" onClick={check}>
          {t("common.retry")}
        </button>
      </div>
    );
  return (
    <TooltipProvider delay={400}>
      <Shell />
    </TooltipProvider>
  );
}
