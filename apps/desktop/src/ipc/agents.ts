import { controlRequest } from "./controlClient";

export interface DeliveryStates {
  implemented: boolean;
  reviewed: boolean;
  tested: boolean;
  complete: boolean;
  integrated: boolean;
  packaged: boolean;
  installed: boolean;
  liveVerified: boolean;
}

export interface AgentStatus {
  agentSessionId: string;
  runtimeState: string;
  workStage: string;
  deliveryStates: DeliveryStates | null;
}

export function getAgent(agentSessionId: string): Promise<AgentStatus> {
  return controlRequest("get_agent", { agentSessionId }) as Promise<AgentStatus>;
}

export const DELIVERY_STATE_LABELS: ReadonlyArray<{
  key: keyof DeliveryStates;
  label: string;
}> = [
  { key: "implemented", label: "implemented" },
  { key: "reviewed", label: "reviewed" },
  { key: "tested", label: "tested" },
  { key: "complete", label: "complete" },
  { key: "integrated", label: "integrated" },
  { key: "packaged", label: "packaged" },
  { key: "installed", label: "installed" },
  { key: "liveVerified", label: "live-verified" },
];
