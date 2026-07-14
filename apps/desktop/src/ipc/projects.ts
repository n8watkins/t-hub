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

export function registerProject(input: {
  repoRoot: string;
  name?: string;
  remoteUrl?: string;
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
