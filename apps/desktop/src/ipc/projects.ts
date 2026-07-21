import { controlRequest } from "./controlClient";

export interface RegisteredProject {
  projectId: string;
  name: string;
  repoRoot: string;
  rootPath?: string;
  vcsCapability?: "git" | "none";
  gitMainRoot?: string;
  remoteUrl?: string;
  defaultBranch?: string;
  createdAt: number;
  updatedAt: number;
}

export interface ProjectCatalog {
  projects: RegisteredProject[];
  count: number;
  seq: number;
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
  createDirectory?: boolean;
}): Promise<RegisteredProject> {
  return controlRequest("register_project", input) as Promise<RegisteredProject>;
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
  agents?: unknown[];
  agentCount?: number;
  agentDigest?: string;
  agentEventCursor?: string;
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
