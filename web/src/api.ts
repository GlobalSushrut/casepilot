export type CaseRecord = {
  case_id: string;
  customer_id: string;
  status: string;
  generation: number;
  summary: string;
  created_at: string;
  workbench_session_id: string | null;
  coordination_task_id: string | null;
};

export type CaseDetail = {
  case: CaseRecord;
  assignments: Array<{ role: string; connector_pid: string | null; session_id: string | null; generation: number }>;
  actions: Array<{
    action_id: string;
    effect_type: string;
    status: string;
    amount_cents: number | null;
    receipt_id: string | null;
    connector_task_id: string | null;
    observed_state: string | null;
  }>;
  plan: { version: number; proposed_resolution: string } | null;
  outcome: { verified_status: string; evidence_refs: string; updated_at: string } | null;
  coordination?: { ok: boolean; error?: string };
};

export type ActionResult = {
  result: "admitted" | "denied" | "approval_required";
  reason: string;
  verified_status: string | null;
  action_id: string;
  observed: unknown;
};

let surface = "";

export function useSurface(value: "report" | "desk" | "review") {
  surface = value;
}

function surfaceHeaders(extra?: Record<string, string>): HeadersInit {
  const headers: Record<string, string> = { ...(extra || {}) };
  if (surface) headers["x-casepilot-surface"] = surface;
  return headers;
}

async function parse<T>(response: Response): Promise<T> {
  const body = (await response.json()) as T & { error?: string };
  if (!response.ok) {
    throw new Error(body.error || response.statusText);
  }
  return body;
}

export function getJson<T>(path: string): Promise<T> {
  return fetch(path, { credentials: "same-origin", headers: surfaceHeaders() }).then((response) => parse<T>(response));
}

export function postJson<T>(path: string, body?: unknown): Promise<T> {
  return fetch(path, {
    method: "POST",
    credentials: "same-origin",
    headers: surfaceHeaders({ "content-type": "application/json" }),
    body: JSON.stringify(body ?? {}),
  }).then((response) => parse<T>(response));
}

export function signIn(role: "customer" | "operator" | "supervisor", principal: string, customerId: string) {
  return postJson<{ role: string; principal: string; customer_id: string }>("/api/casepilot/session", {
    role,
    principal,
    customer_id: customerId,
    surface,
  });
}

export function outcomeLabel(detail: CaseDetail): string {
  const status = detail.outcome?.verified_status;
  if (status === "verified") {
    return "Verified business outcome";
  }
  if (!status) {
    return "No verified outcome yet";
  }
  return status;
}
