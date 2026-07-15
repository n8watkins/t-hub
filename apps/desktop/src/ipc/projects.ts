import { controlRequest } from "./controlClient";

export interface PowderProjectBinding {
  connectionProfile: string;
  repository: string;
  eventCursor?: number;
}

export interface RegisteredProject {
  projectId: string;
  name: string;
  repoRoot: string;
  remoteUrl?: string;
  initializeGit?: boolean;
  defaultBranch?: string;
  powder?: PowderProjectBinding;
  createdAt: number;
  updatedAt: number;
}

export interface ProjectCatalog {
  projects: RegisteredProject[];
  count: number;
  seq: number;
  powderProfiles?: string[];
  powderProfilesError?: string;
  wslHome?: string;
  wslHomeError?: string;
}

export function listProjects(): Promise<ProjectCatalog> {
  return controlRequest("list_projects") as Promise<ProjectCatalog>;
}

export interface PowderBoard {
  name: string;
  aliases: string[];
  tier: string;
  cardCount: number;
}

export interface PowderBoardCatalog {
  connectionProfile: string;
  boards: PowderBoard[];
  count: number;
  totalCount: number;
  hasMore: boolean;
  nextOffset?: number;
}

export type ProjectBoardStatus =
  | "ready"
  | "noProject"
  | "unbound"
  | "unauthorized"
  | "unreachable"
  | "misconfigured"
  | "repositoryMissing"
  | "degraded"
  | "error";

export interface ProjectBoardSnapshot {
  schemaVersion: 1;
  status: ProjectBoardStatus;
  resolution: "captain" | "crew" | "cwd" | "none";
  project?: Pick<RegisteredProject, "projectId" | "name" | "repoRoot">;
  binding?: { repository: string; connectionProfile: string };
  board?: {
    repository: PowderBoard;
    cards: Array<{
      id: string;
      title: string;
      status: string;
      priority: string;
      estimate?: string;
      labels: string[];
      claim?: { agent: string; expiresAt: number };
      updatedAt: number;
    }>;
    totalCount: number;
    hasMore: boolean;
    refreshedAt: number;
  };
  external?: { url: string; repositoryFilterApplied: false };
  problem?: { code: string; message: string; retryable: boolean };
}

export function listPowderBoards(input: {
  connectionProfile: string;
  offset?: number;
  limit?: number;
}): Promise<PowderBoardCatalog> {
  return controlRequest("list_powder_boards", input) as Promise<PowderBoardCatalog>;
}

export function projectBoardSnapshot(input: {
  terminalId: string;
  cwd?: string;
  limit?: number;
}): Promise<ProjectBoardSnapshot> {
  return controlRequest("project_board_snapshot", input) as Promise<ProjectBoardSnapshot>;
}

export function registerProject(input: {
  repoRoot: string;
  name?: string;
  remoteUrl?: string;
  createDirectory?: boolean;
  initializeGit?: boolean;
  powderRepository?: string;
  powderConnectionProfile?: string;
}): Promise<RegisteredProject> {
  return controlRequest("register_project", input) as Promise<RegisteredProject>;
}

export function bindProjectPowder(input: {
  projectId: string;
  repository: string;
  connectionProfile?: string;
}): Promise<RegisteredProject> {
  return controlRequest("bind_project_powder", input) as Promise<RegisteredProject>;
}

export interface CaptainIdentity {
  shipSlug: string;
  terminalId?: string;
  projectId?: string;
  assignment?: string;
  harness?: "codex" | "claude";
  workspaceTabIds: string[];
  crew: unknown[];
}

export interface CaptainBootstrap {
  captain: CaptainIdentity;
  project: RegisteredProject;
  instructions: string;
  recoverySource?: "captains-registry";
}

export function captainBootstrap(input: {
  shipSlug?: string;
  captainSessionId?: string;
}): Promise<CaptainBootstrap> {
  return controlRequest("captain_bootstrap", input) as Promise<CaptainBootstrap>;
}

export function commissionCaptain(input: {
  projectId: string;
  assignment: string;
  harness?: "codex" | "claude";
  shipSlug?: string;
  workspaceTabIds?: string[];
}): Promise<CaptainBootstrap & { alreadyCommissioned: boolean }> {
  return controlRequest("commission_captain", input) as Promise<
    CaptainBootstrap & { alreadyCommissioned: boolean }
  >;
}

export interface CrewIdentity {
  terminalId: string;
  task?: string;
  harness?: "codex" | "claude";
  worktreePath?: string;
  branch?: string;
  powderWork?: {
    cardId: string;
    runId: string;
    claimExpiresAt?: number;
  };
}

export function powderStatus(projectId: string): Promise<{
  projectId: string;
  repository: string;
  connectionProfile: string;
  health: unknown;
}> {
  return controlRequest("powder_status", { projectId }) as Promise<{
    projectId: string;
    repository: string;
    connectionProfile: string;
    health: unknown;
  }>;
}

export function dispatchCrew(input: {
  captainSessionId?: string;
  shipSlug?: string;
  cardId: string;
  task: string;
  harness?: "codex" | "claude";
  worktreePath?: string;
  branch?: string;
  ttlSeconds?: number;
  tabId?: string;
  tabName?: string;
}): Promise<{
  captain: CaptainIdentity;
  crew: CrewIdentity;
  project: RegisteredProject;
  powderCard: unknown;
}> {
  return controlRequest("dispatch_crew", input) as Promise<{
    captain: CaptainIdentity;
    crew: CrewIdentity;
    project: RegisteredProject;
    powderCard: unknown;
  }>;
}

export function heartbeatCrewPowder(crewSessionId: string): Promise<{
  crew: CrewIdentity;
}> {
  return controlRequest("heartbeat_crew_powder", { crewSessionId }) as Promise<{
    crew: CrewIdentity;
  }>;
}
