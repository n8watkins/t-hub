export interface DispatchIntegrationContract {
  contractId: string;
  integrationOwner: string;
  orderedLaneIds: string[];
}

export interface DispatchClaims {
  laneId: string;
  dependencies: string[];
  mutableFiles: string[];
  mutableSchemas: string[];
  mutableInterfaces: string[];
  integrationContracts: DispatchIntegrationContract[];
}

export interface DispatchClaimInputs {
  laneId: string;
  dependencies: string;
  mutableFiles: string;
  mutableSchemas: string;
  mutableInterfaces: string;
  integrationContracts: string;
}

const MAX_CLAIMS = 128;
const STABLE_ID = /^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$/;

function uniqueLines(value: string, label: string): string[] {
  const lines = value
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  if (lines.length > MAX_CLAIMS) throw new Error(`${label} supports at most ${MAX_CLAIMS} entries.`);
  return [...new Set(lines)];
}

function stableId(value: string, label: string): string {
  const normalized = value.trim();
  if (!STABLE_ID.test(normalized)) {
    throw new Error(
      `${label} must start with a letter or number and use only letters, numbers, dot, colon, slash, underscore, or hyphen.`,
    );
  }
  return normalized;
}

function stableIds(value: string, label: string): string[] {
  return uniqueLines(value, label).map((entry) => stableId(entry, `${label} entry '${entry}'`));
}

export function normalizeRepositoryResource(value: string): string {
  const replaced = value.trim().replace(/\\/g, "/");
  if (!replaced || replaced.startsWith("/") || /^[A-Za-z]:\//.test(replaced)) {
    throw new Error(`Mutable file '${value}' must be relative to the repository root.`);
  }
  if (/[*?[\]]/.test(replaced)) {
    throw new Error(`Mutable file '${value}' must be a path or directory prefix, not a glob.`);
  }
  const components: string[] = [];
  for (const component of replaced.split("/")) {
    if (!component || component === ".") continue;
    if (component === "..") {
      throw new Error(`Mutable file '${value}' cannot traverse outside the repository root.`);
    }
    components.push(component);
  }
  if (components.length === 0) {
    throw new Error(`Mutable file '${value}' does not name a repository resource.`);
  }
  return components.join("/");
}

function repositoryResources(value: string): string[] {
  const resources = uniqueLines(value, "Mutable files").map(normalizeRepositoryResource);
  return [...new Set(resources)];
}

function integrationContracts(value: string, laneId: string): DispatchIntegrationContract[] {
  return uniqueLines(value, "Integration contracts").map((line, index) => {
    const fields = line.split("|").map((field) => field.trim());
    if (fields.length !== 3) {
      throw new Error(
        `Integration contract line ${index + 1} must use: contract-id | owner-id | lane-a, lane-b.`,
      );
    }
    const contractId = stableId(fields[0], `Integration contract line ${index + 1} ID`);
    const integrationOwner = stableId(fields[1], `Integration contract line ${index + 1} owner`);
    const orderedLaneIds = fields[2]
      .split(",")
      .map((entry) => stableId(entry, `Integration contract '${contractId}' lane`));
    if (orderedLaneIds.length < 2 || new Set(orderedLaneIds).size !== orderedLaneIds.length) {
      throw new Error(
        `Integration contract '${contractId}' requires at least two unique ordered lane IDs.`,
      );
    }
    if (!orderedLaneIds.includes(laneId)) {
      throw new Error(`Integration contract '${contractId}' must include this lane '${laneId}'.`);
    }
    return { contractId, integrationOwner, orderedLaneIds };
  });
}

export function parseDispatchClaims(inputs: DispatchClaimInputs): DispatchClaims {
  const laneId = stableId(inputs.laneId, "Lane ID");
  const dependencies = stableIds(inputs.dependencies, "Dependencies");
  if (dependencies.includes(laneId)) throw new Error("A lane cannot depend on itself.");
  const mutableFiles = repositoryResources(inputs.mutableFiles);
  const mutableSchemas = stableIds(inputs.mutableSchemas, "Mutable schemas");
  const mutableInterfaces = stableIds(inputs.mutableInterfaces, "Mutable interfaces");
  if (mutableFiles.length + mutableSchemas.length + mutableInterfaces.length === 0) {
    throw new Error("Claim at least one mutable file, schema, or interface for this implementation lane.");
  }
  return {
    laneId,
    dependencies,
    mutableFiles,
    mutableSchemas,
    mutableInterfaces,
    integrationContracts: integrationContracts(inputs.integrationContracts, laneId),
  };
}
