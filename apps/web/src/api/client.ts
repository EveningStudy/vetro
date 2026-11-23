import { PROTOCOL_VERSION, type AuthApi, type Transport } from "./protocol";
import { WsTransport, httpAuth, wsUrl } from "./ws";

export const isMock =
  import.meta.env.VITE_MOCK === "1" || new URLSearchParams(location.search).get("mock") === "1";

let transport: Transport | null = null;
let auth: AuthApi = httpAuth;

/** Picks the real core or the in-browser mock once, before anything else runs. */
export async function initClient() {
  if (isMock) {
    const mock = await import("~/mock/MockTransport");
    transport = new mock.MockTransport();
    auth = mock.mockAuth;
  } else {
    transport = new WsTransport(wsUrl(), { clientName: "vetro-web", protocolVersion: PROTOCOL_VERSION });
  }
}

export function client(): Transport {
  if (!transport) throw new Error("client not initialised");
  return transport;
}

export function authApi(): AuthApi {
  return auth;
}
