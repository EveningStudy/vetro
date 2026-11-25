import type { AgentOptionsParams } from "./bindings/AgentOptionsParams";
import type { AgentOptionsResult } from "./bindings/AgentOptionsResult";
import type { AgentsResult } from "./bindings/AgentsResult";
import type { AppInfo } from "./bindings/AppInfo";
import type { AuthenticateParams } from "./bindings/AuthenticateParams";
import type { AuthenticateResult } from "./bindings/AuthenticateResult";
import type { Empty } from "./bindings/Empty";
import type { FsFindParams } from "./bindings/FsFindParams";
import type { FsFindResult } from "./bindings/FsFindResult";
import type { FsListParams } from "./bindings/FsListParams";
import type { FsListResult } from "./bindings/FsListResult";
import type { FsMkdirParams } from "./bindings/FsMkdirParams";
import type { FsMkdirResult } from "./bindings/FsMkdirResult";
import type { GitDiffParams } from "./bindings/GitDiffParams";
import type { GitDiffResult } from "./bindings/GitDiffResult";
import type { GitStatusResult } from "./bindings/GitStatusResult";
import type { HelloParams } from "./bindings/HelloParams";
import type { NotifyParams } from "./bindings/NotifyParams";
import type { OpenExternalParams } from "./bindings/OpenExternalParams";
import type { OptionsResult } from "./bindings/OptionsResult";
import type { Pairing } from "./bindings/Pairing";
import type { Prefs } from "./bindings/Prefs";
import type { Project } from "./bindings/Project";
import type { ProjectAddParams } from "./bindings/ProjectAddParams";
import type { ProjectUpdateParams } from "./bindings/ProjectUpdateParams";
import type { ProjectsResult } from "./bindings/ProjectsResult";
import type { RemoteRevokeParams } from "./bindings/RemoteRevokeParams";
import type { RemoteSetParams } from "./bindings/RemoteSetParams";
import type { RemoteStatus } from "./bindings/RemoteStatus";
import type { RewindFilesResult } from "./bindings/RewindFilesResult";
import type { SearchParams } from "./bindings/SearchParams";
import type { SearchResult } from "./bindings/SearchResult";
import type { ServerEvent } from "./bindings/ServerEvent";
import type { Settings } from "./bindings/Settings";
import type { SettingsUpdateParams } from "./bindings/SettingsUpdateParams";
import type { ThreadForkParams } from "./bindings/ThreadForkParams";
import type { ThreadIdParams } from "./bindings/ThreadIdParams";
import type { ThreadItemsParams } from "./bindings/ThreadItemsParams";
import type { ThreadItemsResult } from "./bindings/ThreadItemsResult";
import type { ThreadOpenParams } from "./bindings/ThreadOpenParams";
import type { ThreadOpenResult } from "./bindings/ThreadOpenResult";
import type { ThreadQueueCancelParams } from "./bindings/ThreadQueueCancelParams";
import type { ThreadRawParams } from "./bindings/ThreadRawParams";
import type { ThreadRawResult } from "./bindings/ThreadRawResult";
import type { ThreadRespondParams } from "./bindings/ThreadRespondParams";
import type { ThreadResult } from "./bindings/ThreadResult";
import type { ThreadRewindFilesParams } from "./bindings/ThreadRewindFilesParams";
import type { ThreadRollbackParams } from "./bindings/ThreadRollbackParams";
import type { ThreadSendParams } from "./bindings/ThreadSendParams";
import type { ThreadSetOptionParams } from "./bindings/ThreadSetOptionParams";
import type { ThreadStartParams } from "./bindings/ThreadStartParams";
import type { ThreadUpdateParams } from "./bindings/ThreadUpdateParams";
import type { ThreadsListParams } from "./bindings/ThreadsListParams";
import type { ThreadsListResult } from "./bindings/ThreadsListResult";
import type { VoiceModelParams } from "./bindings/VoiceModelParams";
import type { VoiceStatus } from "./bindings/VoiceStatus";
import type { VoiceTranscribeParams } from "./bindings/VoiceTranscribeParams";
import type { VoiceTranscribeResult } from "./bindings/VoiceTranscribeResult";
import type { WorkspaceFileParams } from "./bindings/WorkspaceFileParams";
import type { WorkspaceFileResult } from "./bindings/WorkspaceFileResult";
import type { WorkspacePathParams } from "./bindings/WorkspacePathParams";
import type { WorktreesResult } from "./bindings/WorktreesResult";

export const PROTOCOL_VERSION = 1;

export interface Methods {
  "app.hello": [HelloParams, AppInfo];
  "agents.list": [Empty, AgentsResult];
  "agents.refresh": [Empty, AgentsResult];
  "agents.authenticate": [AuthenticateParams, AuthenticateResult];
  "agents.options": [AgentOptionsParams, AgentOptionsResult];
  "projects.list": [Empty, ProjectsResult];
  "projects.add": [ProjectAddParams, Project];
  "projects.remove": [ProjectAddParams, Empty];
  "projects.update": [ProjectUpdateParams, Project];
  "threads.list": [ThreadsListParams, ThreadsListResult];
  "threads.open": [ThreadOpenParams, ThreadOpenResult];
  "threads.close": [ThreadIdParams, Empty];
  "threads.items": [ThreadItemsParams, ThreadItemsResult];
  "threads.start": [ThreadStartParams, ThreadResult];
  "threads.send": [ThreadSendParams, Empty];
  "threads.cancelQueued": [ThreadQueueCancelParams, Empty];
  "threads.fork": [ThreadForkParams, ThreadResult];
  "threads.interrupt": [ThreadIdParams, Empty];
  "threads.setOption": [ThreadSetOptionParams, OptionsResult];
  "threads.respond": [ThreadRespondParams, Empty];
  "threads.update": [ThreadUpdateParams, ThreadResult];
  "threads.detach": [ThreadIdParams, Empty];
  "threads.raw": [ThreadRawParams, ThreadRawResult];
  "threads.rollback": [ThreadRollbackParams, ThreadOpenResult];
  "threads.rewindFiles": [ThreadRewindFilesParams, RewindFilesResult];
  "search.threads": [SearchParams, SearchResult];
  "fs.list": [FsListParams, FsListResult];
  "fs.find": [FsFindParams, FsFindResult];
  "fs.mkdir": [FsMkdirParams, FsMkdirResult];
  "workspace.gitStatus": [WorkspacePathParams, GitStatusResult];
  "workspace.gitDiff": [GitDiffParams, GitDiffResult];
  "workspace.readFile": [WorkspaceFileParams, WorkspaceFileResult];
  "workspace.worktrees": [WorkspacePathParams, WorktreesResult];
  "workspace.openExternal": [OpenExternalParams, Empty];
  "app.notify": [NotifyParams, Empty];
  "settings.get": [Empty, Settings];
  "settings.update": [SettingsUpdateParams, Settings];
  "prefs.get": [Empty, Prefs];
  "prefs.set": [Prefs, Empty];
  "remote.status": [Empty, RemoteStatus];
  "remote.set": [RemoteSetParams, RemoteStatus];
  "remote.createPairing": [Empty, Pairing];
  "remote.revokeDevice": [RemoteRevokeParams, RemoteStatus];
  "voice.status": [Empty, VoiceStatus];
  "voice.install": [VoiceModelParams, VoiceStatus];
  "voice.uninstall": [VoiceModelParams, VoiceStatus];
  "voice.transcribe": [VoiceTranscribeParams, VoiceTranscribeResult];
}

export type Method = keyof Methods;
export type Params<M extends Method> = Methods[M][0];
export type Result<M extends Method> = Methods[M][1];

export type ConnectionStatus = "connecting" | "open" | "closed";

export type StatusListener = (
  status: ConnectionStatus,
  detail: { info: AppInfo | null; reconnected: boolean },
) => void;

export class RpcFailure extends Error {
  constructor(
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

/** One connection to a Vetro core, real or mocked. */
export interface Transport {
  request<M extends Method>(method: M, params: Params<M>): Promise<Result<M>>;
  onEvent(listener: (event: ServerEvent) => void): () => void;
  onStatus(listener: StatusListener): () => void;
  status(): ConnectionStatus;
  start(): void;
}

export interface AuthApi {
  session(): Promise<{ authenticated: boolean; remoteClient: boolean } | null>;
  local(token: string): Promise<{ ok: boolean; remoteClient?: boolean }>;
  pair(code: string, deviceName: string): Promise<{ ok: boolean; message?: string }>;
}
