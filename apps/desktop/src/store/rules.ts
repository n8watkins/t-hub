// Event→action RULES engine (WS-5b) — the user-configurable generalization of
// the today-hardcoded autocontinue/notify wiring.
//
// A Rule fires when a SUPERVISED session's FR-012 status TRANSITIONS into a
// target status (optionally only from a specific previous status), and runs one
// configured action — notify, type text into the session's terminal, spawn a new
// terminal, restart the session's terminal, or run a command in it. This store
// holds the rule LIST + CRUD; the timing/firing lives in src/lib/rulesMount.ts
// (a side-effect mount that subscribes to the session-status events, exactly like
// notify.ts / autoContinueMount.ts).
//
// Persistence is localStorage under a versioned key (mirrors store/settings.ts):
// the rule list is plain JSON and survives an app restart. Unknown/corrupt shapes
// fall back to the built-in defaults so a bad write can never wedge the engine.
import { create } from "zustand";
import type { SessionStatus } from "../ipc/model";
import { loadPersisted, savePersisted } from "../lib/persist";

const STORAGE_KEY = "t-hub.rules.v1";

/** The FR-012 statuses a rule can trigger on. These are exactly the camelCase
 *  reducer/overlay statuses the bridge emits (ipc/model.ts `SessionStatus`) — we
 *  surface only the ones a user would meaningfully react to (the routine
 *  `working`/`detached`/`restoring`/`unknown` make poor triggers but are still
 *  valid values, so the engine compares against the raw string regardless). */
export const TRIGGER_STATUSES: SessionStatus[] = [
  "completed",
  "failed",
  "needsQuestion",
  "needsPermission",
  "waitingOnSubagents",
  "rateLimited",
  "working",
];

/** Friendly label for a status, for the Settings UI dropdowns. */
export function statusLabel(status: SessionStatus | "any"): string {
  switch (status) {
    case "any":
      return "Any";
    case "completed":
      return "Completed";
    case "failed":
      return "Failed";
    case "needsQuestion":
      return "Needs question";
    case "needsPermission":
      return "Needs permission";
    case "waitingOnSubagents":
      return "Waiting on subagents";
    case "rateLimited":
      return "Rate limited";
    case "working":
      return "Working";
    case "detached":
      return "Detached";
    case "restoring":
      return "Restoring";
    case "expired":
      return "Expired";
    case "unknown":
      return "Unknown";
  }
}

/** The kinds of action a rule can run. Each composes from primitives that already
 *  exist on the frontend (writeTerminal / spawnTerminal / killTerminal + notify),
 *  so the engine needs NO new backend command. */
export type ActionKind = "notify" | "sendText" | "spawn" | "restart" | "run";

export const ACTION_KINDS: ActionKind[] = [
  "notify",
  "sendText",
  "spawn",
  "restart",
  "run",
];

/** Friendly label for an action kind, for the Settings UI. */
export function actionKindLabel(kind: ActionKind): string {
  switch (kind) {
    case "notify":
      return "Notify (sound + desktop)";
    case "sendText":
      return "Send text to the session";
    case "spawn":
      return "Spawn a new terminal";
    case "restart":
      return "Restart the session's terminal";
    case "run":
      return "Run a command in the session";
  }
}

/** The action a rule runs, plus the params each kind needs. A single flat shape
 *  (rather than a discriminated union per kind) keeps the localStorage codec and
 *  the Settings builder simple — unused fields for a given `kind` are just
 *  ignored. `text` doubles as the notify body / the typed text / the command. */
export interface RuleAction {
  kind: ActionKind;
  /** notify: the body shown; sendText/run: the text/command typed into the PTY;
   *  unused by spawn/restart. */
  text?: string;
  /** spawn/run: the working directory for the new terminal. Empty = inherit from
   *  the triggering session's terminal (its own cwd), falling back to the default
   *  shell cwd when that can't be resolved. */
  cwd?: string;
}

/** What makes a rule fire: a target status the session must transition INTO, and
 *  optionally the previous status it must transition FROM ("any" = don't care). */
export interface RuleTrigger {
  /** The status the session must enter for the rule to fire. */
  to: SessionStatus;
  /** Only fire when the PREVIOUS status was exactly this (else "any"). Lets a rule
   *  target a specific transition, e.g. working→completed but not failed→completed. */
  from: SessionStatus | "any";
}

export interface Rule {
  id: string;
  /** User-facing name (purely cosmetic; the engine keys off `id`). */
  name: string;
  enabled: boolean;
  trigger: RuleTrigger;
  action: RuleAction;
}

/** A short, collision-resistant id for a new rule. crypto.randomUUID when
 *  available; a timestamp+random fallback otherwise (keeps tests/SSR working). */
function newId(): string {
  try {
    if (typeof crypto !== "undefined" && crypto.randomUUID) {
      return crypto.randomUUID();
    }
  } catch {
    /* fall through to the manual id */
  }
  return `rule_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 8)}`;
}

/** A fresh rule with sensible defaults (the Settings "Add rule" button uses
 *  this): a disabled "notify when a session completes" rule the user then edits. */
export function defaultRule(): Rule {
  return {
    id: newId(),
    name: "New rule",
    enabled: false,
    trigger: { to: "completed", from: "any" },
    action: { kind: "notify", text: "A session changed status." },
  };
}

/** Built-in starter rules, seeded the FIRST time the app runs (no persisted key
 *  yet). They are ordinary, fully-editable rules — the user can disable or delete
 *  them. We ship the headline WS-5b example DISABLED so nothing fires until the
 *  user opts in (a spawn-on-end that fired unbidden would be a surprise). */
function builtinRules(): Rule[] {
  return [
    {
      id: "builtin.spawn-on-end",
      name: "Open a terminal when a session ends",
      enabled: false,
      trigger: { to: "completed", from: "any" },
      action: { kind: "spawn", text: "", cwd: "" },
    },
  ];
}

/** Coerce one unknown persisted entry into a valid Rule, or null to drop it. We
 *  validate defensively: a hand-edited / version-skewed localStorage blob must
 *  never throw into the engine, only lose the bad entries. */
function coerceRule(raw: unknown): Rule | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const id = typeof r.id === "string" && r.id ? r.id : newId();
  const name = typeof r.name === "string" ? r.name : "Rule";
  const enabled = r.enabled === true;

  const t = (r.trigger ?? {}) as Record<string, unknown>;
  const to = typeof t.to === "string" ? (t.to as SessionStatus) : "completed";
  const from =
    typeof t.from === "string" ? (t.from as SessionStatus | "any") : "any";

  const a = (r.action ?? {}) as Record<string, unknown>;
  const kind = ACTION_KINDS.includes(a.kind as ActionKind)
    ? (a.kind as ActionKind)
    : "notify";
  const text = typeof a.text === "string" ? a.text : undefined;
  const cwd = typeof a.cwd === "string" ? a.cwd : undefined;

  return { id, name, enabled, trigger: { to, from }, action: { kind, text, cwd } };
}

function load(): Rule[] {
  // SSR / absent key / corrupt blob => the built-in starters (the fallback);
  // a parsed-but-non-array blob => an empty list (the coerce decision below).
  // The SSR guard + getItem + corrupt-fallback plumbing lives in lib/persist.
  return loadPersisted(STORAGE_KEY, builtinRules(), (parsed): Rule[] => {
    if (!Array.isArray(parsed)) return [];
    return parsed.map(coerceRule).filter((r): r is Rule => r !== null);
  });
}

function save(rules: Rule[]): void {
  savePersisted(STORAGE_KEY, rules);
}

interface RulesState {
  /** The ordered rule list (render order in Settings; firing order is irrelevant
   *  since each rule is independent). */
  rules: Rule[];
  /** Append a new (disabled) rule and return its id. */
  add: () => string;
  /** Remove a rule by id. */
  remove: (id: string) => void;
  /** Flip a rule's enabled flag. */
  toggle: (id: string) => void;
  /** Patch a rule in place (shallow merge of top-level fields; trigger/action are
   *  replaced wholesale by the caller). No-op for an unknown id. */
  update: (id: string, patch: Partial<Omit<Rule, "id">>) => void;
}

export const useRules = create<RulesState>((set) => ({
  rules: load(),

  add: () => {
    const rule = defaultRule();
    set((s) => {
      const rules = [...s.rules, rule];
      save(rules);
      return { rules };
    });
    return rule.id;
  },

  remove: (id) =>
    set((s) => {
      const rules = s.rules.filter((r) => r.id !== id);
      save(rules);
      return { rules };
    }),

  toggle: (id) =>
    set((s) => {
      const rules = s.rules.map((r) =>
        r.id === id ? { ...r, enabled: !r.enabled } : r,
      );
      save(rules);
      return { rules };
    }),

  update: (id, patch) =>
    set((s) => {
      const rules = s.rules.map((r) => (r.id === id ? { ...r, ...patch } : r));
      save(rules);
      return { rules };
    }),
}));
