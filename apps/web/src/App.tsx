import { useEffect, useState } from "react";
import { authApi } from "~/api/client";
import { useLang, useT } from "~/i18n";
import { PairingPage } from "./components/PairingPage";
import { Shell } from "./components/Shell";
import { TooltipProvider } from "./components/ui/overlay";
import { Spinner } from "./components/ui/primitives";
import { startSync } from "./state/store";

type AuthState = "checking" | "ok" | "pair" | "error";

/** The desktop window receives a one-time local token as `/#token=...`; trade it for a cookie and
 * drop it from the address bar before anything else can read it. */
async function exchangeDesktopToken() {
  const match = /^#token=([^&]+)/.exec(location.hash);
  if (!match) return;
  history.replaceState(null, "", `${location.pathname}${location.search}`);
  await authApi().local(decodeURIComponent(match[1]!));
}

export function App() {
  const t = useT();
  const lang = useLang();
  const [auth, setAuth] = useState<AuthState>("checking");

  useEffect(() => {
    document.documentElement.lang = lang === "zh" ? "zh-CN" : "en";
  }, [lang]);

  const check = () => {
    setAuth("checking");
    exchangeDesktopToken()
      .then(() => authApi().session())
      .then((session) => setAuth(session ? "ok" : "pair"))
      .catch(() => setAuth("error"));
  };

  useEffect(check, []);

  useEffect(() => {
    if (auth === "ok") startSync();
  }, [auth]);

  if (auth === "checking")
    return (
      <div className="flex h-dvh items-center justify-center">
        <Spinner className="text-muted-foreground" />
      </div>
    );
  if (auth === "pair") return <PairingPage onPaired={() => setAuth("ok")} />;
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
