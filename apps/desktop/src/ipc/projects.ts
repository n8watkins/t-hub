import { controlRequest } from "./controlClient";

export interface PowderProjectBinding {
  connectionProfile: string;
  repository: string;
}

export interface RegisteredProject {
  projectId: string;
  name: string;
  repoRoot: string;
  remoteUrl?: string;
  defaultBranch?: string;
  powder?: PowderProjectBinding;
  createdAt: number;
  updatedAt: number;
}

export interface ProjectCatalog {
  projects: RegisteredProject[];
  count: number;
  seq: number;
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
