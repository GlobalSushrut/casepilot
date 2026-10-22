import { getJson, postJson, signIn, useSurface } from "./api.js";

useSurface("review");

void getJson<{ account?: string | null; ready: boolean }>("/api/casepilot/setup/status").then((gate) => {
  if (!gate.account || !gate.ready) window.location.replace("/");
});

const root = document.querySelector<HTMLElement>("#app") ?? missing();
function missing(): never {
  throw new Error("missing app");
}

type CustomerRow = {
  customer_id: string;
  case_count: number;
  awaiting_human_count: number;
  summary: string;
  last_activity_at?: string | null;
  attention?: string | null;
};

type CaseItem = {
  case_id: string;
  summary: string;
  status: string;
  worker_id?: string | null;
  release_phase?: string | null;
  confirmed_position?: string | null;
  actual_customer_reply?: string | null;
  created_at?: string;
};

let signedIn = false;
let sort = "latest";
let offset = 0;
let query = "";
let total = 0;
let rows: CustomerRow[] = [];
let highlight = 0;
let customerId = "";
let selected = "";
let dirty = false;

root.innerHTML = `
  <header class="top">
    <div class="brand">CasePilot<small>Review</small></div>
    <nav class="links"><a href="/portal">Report</a><a href="/workstation">Desk</a></nav>
  </header>
  <main class="review">
    <aside class="review-queue">
      <div class="pane-head">
        <h2>Customers</h2>
        <input id="q" placeholder="Search customer or complaint" aria-label="Search customers">
        <div class="segment" id="sorts">
          <button type="button" data-sort="latest">Latest</button>
          <button type="button" data-sort="hot">Hot</button>
          <button type="button" data-sort="all">All</button>
        </div>
        <p id="queueMeta" class="muted"></p>
      </div>
      <div id="customers" class="queue-list"></div>
      <div class="pane-foot" id="pager"></div>
    </aside>
    <section class="review-record">
      <div id="record"><p class="empty">Select a customer. Their complaints stay separate.</p></div>
    </section>
    <section class="review-case">
      <header class="case-bar" id="caseHead">
        <div>
          <h2 id="caseTitle">No complaint open</h2>
          <p id="caseMeta" class="muted">Choose one from the customer record.</p>
        </div>
        <span id="caseStatus" class="status idle">Waiting</span>
      </header>
      <div class="review-split">
        <section class="chat-pane">
          <header>Customer</header>
          <div id="customerThread" class="thread"></div>
        </section>
        <section class="chat-pane">
          <header>Worker</header>
          <div id="workerThread" class="thread"></div>
          <form id="workerChat">
            <input name="message" placeholder="Ask the worker about this complaint" aria-label="Ask the worker">
            <button type="submit">Ask</button>
          </form>
        </section>
      </div>
      <footer class="decision">
        <label>Organizational position
          <textarea id="releaseText" placeholder="The decision. The customer agent writes the sentence the customer reads."></textarea>
        </label>
        <div class="actions">
          <button type="button" id="saveDraft" class="ghost">Save draft</button>
          <button type="button" id="release">Confirm position</button>
          <span id="phase" role="status" class="muted"></span>
        </div>
      </footer>
      <details class="tools">
        <summary>Order, money, and catalog</summary>
        <div id="ops"></div>
        <div class="actions">
          <button type="button" id="readOrder" class="ghost">Read the order</button>
          <button type="button" id="cease" class="ghost">Cease</button>
          <button type="button" id="resume" class="ghost">Resume</button>
        </div>
        <p class="eyebrow">Money approvals</p>
        <div id="approvals"></div>
        <form id="catalog"></form>
        <pre id="deploy"></pre>
      </details>
    </section>
  </main>
`;

const customers = root.querySelector<HTMLElement>("#customers");
const record = root.querySelector<HTMLElement>("#record");
const phase = root.querySelector<HTMLElement>("#phase");
const ops = root.querySelector<HTMLElement>("#ops");
const approvals = root.querySelector<HTMLElement>("#approvals");
const deploy = root.querySelector<HTMLElement>("#deploy");
const position = root.querySelector<HTMLTextAreaElement>("#releaseText");

root.querySelector("#sorts")?.addEventListener("click", (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-sort]");
  if (!button?.dataset.sort) return;
  sort = button.dataset.sort;
  offset = 0;
  void loadCustomers();
});

root.querySelector<HTMLInputElement>("#q")?.addEventListener("change", (event) => {
  query = (event.target as HTMLInputElement).value.trim();
  offset = 0;
  void loadCustomers();
});

customers?.addEventListener("click", (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-customer]");
  if (!button?.dataset.customer) return;
  highlight = Number(button.dataset.index || "0");
  void openCustomer(button.dataset.customer);
});

record?.addEventListener("click", (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-case]");
  if (!button?.dataset.case) return;
  void openCase(button.dataset.case);
});

position?.addEventListener("input", () => {
  dirty = true;
});

root.querySelector("#saveDraft")?.addEventListener("click", () => void saveDraft());
root.querySelector("#release")?.addEventListener("click", () => void confirmPosition());
root.querySelector("#readOrder")?.addEventListener("click", () => void readOrder());
root.querySelector("#cease")?.addEventListener("click", () => void act("cease"));
root.querySelector("#resume")?.addEventListener("click", () => void act("resume"));

root.querySelector<HTMLFormElement>("#workerChat")?.addEventListener("submit", async (event) => {
  event.preventDefault();
  if (!selected) return;
  const form = event.target as HTMLFormElement;
  const message = String(new FormData(form).get("message") || "").trim();
  if (!message) return;
  const thread = root.querySelector<HTMLElement>("#workerThread");
  thread?.insertAdjacentHTML("beforeend", hint("The worker is reading this."));
  try {
    const body = await postJson<{ reply: string }>(`/api/casepilot/cases/${selected}/chat`, { message });
    form.reset();
    thread?.insertAdjacentHTML("beforeend", bubble("you", "You", message) + bubble("agent", "Worker", body.reply));
  } catch (error) {
    thread?.insertAdjacentHTML("beforeend", hint(error instanceof Error ? error.message : "The worker did not answer."));
  }
});

approvals?.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-id]");
  if (!button?.dataset.id || !button.dataset.act) return;
  await postJson(`/api/casepilot/approvals/${button.dataset.id}/${button.dataset.act}`, {});
  await loadApprovals();
});

document.addEventListener("keydown", (event) => {
  const target = event.target as HTMLElement | null;
  const typing = !!target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT");
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
    event.preventDefault();
    if (selected) void saveDraft();
    return;
  }
  if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
    event.preventDefault();
    if (target?.id === "workerChat" || target?.closest("#workerChat")) return;
    if (selected) void confirmPosition();
    return;
  }
  if (typing) return;
  if (event.key === "/") {
    event.preventDefault();
    root.querySelector<HTMLInputElement>("#q")?.focus();
  } else if (event.key === "j" || event.key === "J") {
    moveHighlight(1);
  } else if (event.key === "k" || event.key === "K") {
    moveHighlight(-1);
  } else if (event.key === "Enter") {
    const row = rows[highlight];
    if (row) void openCustomer(row.customer_id);
  } else if (event.key === "Escape") {
    selected = "";
    const title = root.querySelector<HTMLElement>("#caseTitle");
    const meta = root.querySelector<HTMLElement>("#caseMeta");
    const badge = root.querySelector<HTMLElement>("#caseStatus");
    if (title) title.textContent = "No complaint open";
    if (meta) meta.textContent = "Choose one from the customer record.";
    if (badge) {
      badge.textContent = "Waiting";
      badge.className = "status idle";
    }
    const customerThread = root.querySelector<HTMLElement>("#customerThread");
    const workerThread = root.querySelector<HTMLElement>("#workerThread");
    if (customerThread) customerThread.innerHTML = "";
    if (workerThread) workerThread.innerHTML = "";
  }
});

void openReview("supervisor.lee");

async function openReview(principal: string) {
  const ready = await getJson<{ ready: boolean; reason?: string }>("/api/casepilot/setup/status");
  if (!ready.ready) {
    if (deploy) deploy.textContent = ready.reason || "Connector is not live. Review stays closed.";
    return;
  }
  await signIn("supervisor", principal, "desk");
  signedIn = true;
  await loadCustomers();
  await loadCatalog();
  await loadApprovals();
  const status = await getJson("/api/casepilot/deployment");
  if (deploy) deploy.textContent = JSON.stringify(status, null, 2);
}

async function loadCustomers() {
  const body = await getJson<{
    customers: CustomerRow[];
    total: number;
    offset: number;
    counts: { hot_customers: number; cases_awaiting_human: number };
  }>(`/api/casepilot/work/queue?view=customers&sort=${encodeURIComponent(sort)}&q=${encodeURIComponent(query)}&limit=40&offset=${offset}`);
  rows = body.customers || [];
  total = body.total || 0;
  if (highlight >= rows.length) highlight = 0;
  root.querySelectorAll<HTMLButtonElement>("#sorts button").forEach((button) => {
    button.classList.toggle("active", button.dataset.sort === sort);
  });
  const meta = root.querySelector<HTMLElement>("#queueMeta");
  if (meta) {
    const hot = body.counts?.hot_customers || 0;
    meta.textContent = sort === "hot"
      ? `${hot} customers need attention`
      : `Showing ${rows.length ? offset + 1 : 0}–${offset + rows.length} of ${total}`;
  }
  customers!.innerHTML = rows.map((row, index) => {
    const attention = row.attention ? "Delivery failed" : row.awaiting_human_count ? "Decision required" : "No decision waiting";
    const tone = row.attention ? "bad" : row.awaiting_human_count ? "wait" : "ok";
    const open = row.customer_id === customerId ? "active" : "";
    const cursor = index === highlight ? "cursor" : "";
    return `<button type="button" data-customer="${escapeHtml(row.customer_id)}" data-index="${index}" class="${open} ${cursor}">
      <span class="avatar">${escapeHtml(initials(row.customer_id))}</span>
      <strong>${escapeHtml(personName(row.customer_id))}</strong>
      <small>${escapeHtml(row.summary)}</small>
      <small><span class="status ${tone}">${escapeHtml(attention)}</span> ${row.case_count} cases${row.last_activity_at ? ` · ${escapeHtml(when(row.last_activity_at))}` : ""}</small>
    </button>`;
  }).join("") || `<p class="empty">No customer matches.</p>`;
  const pages = Math.max(1, Math.ceil(total / 40));
  const page = Math.floor(offset / 40) + 1;
  const pager = root.querySelector<HTMLElement>("#pager");
  if (pager) {
    pager.innerHTML = `<button type="button" class="ghost" id="prev" ${page <= 1 ? "disabled" : ""}>Prev</button><span>${page} of ${pages}</span><button type="button" class="ghost" id="next" ${page >= pages ? "disabled" : ""}>Next</button>`;
    pager.querySelector("#prev")?.addEventListener("click", () => {
      offset = Math.max(0, offset - 40);
      void loadCustomers();
    });
    pager.querySelector("#next")?.addEventListener("click", () => {
      if (offset + 40 < total) offset += 40;
      void loadCustomers();
    });
  }
}

function moveHighlight(step: number) {
  if (!rows.length) return;
  highlight = Math.min(rows.length - 1, Math.max(0, highlight + step));
  customers?.querySelectorAll<HTMLButtonElement>("[data-customer]").forEach((button, index) => {
    button.classList.toggle("active", button.dataset.customer === customerId);
    button.classList.toggle("cursor", index === highlight);
  });
}

async function openCustomer(id: string) {
  const switching = customerId !== id;
  customerId = id;
  if (switching) selected = "";
  const body = await getJson<{
    customer_id: string;
    snapshot: string;
    references: string[];
    current: CaseItem[];
    history: CaseItem[];
  }>(`/api/casepilot/customers/${encodeURIComponent(id)}`);
  const refs = (body.references || []).map((item) => `<span class="chip">${escapeHtml(item)}</span>`).join("") || `<span class="muted">None stored</span>`;
  const current = (body.current || []).map(caseButton).join("") || `<p class="empty">No open complaint.</p>`;
  const history = (body.history || []).map((row) => `<article class="past"><header><strong>${escapeHtml(row.summary)}</strong>${statusPill(row)}</header>${row.confirmed_position ? `<p><span>Confirmed</span> ${escapeHtml(row.confirmed_position)}</p>` : ""}${row.actual_customer_reply ? `<p><span>Sent</span> ${escapeHtml(row.actual_customer_reply)}</p>` : ""}</article>`).join("") || `<p class="empty">No earlier complaint.</p>`;
  record!.innerHTML = `<div class="who"><span class="avatar lg">${escapeHtml(initials(body.customer_id))}</span><div><h2>${escapeHtml(personName(body.customer_id))}</h2><p class="muted">${escapeHtml(body.customer_id)}</p></div></div><p class="snapshot">${escapeHtml(body.snapshot)}</p><div class="record-stats"><div><strong>${(body.current || []).length}</strong><span>Open</span></div><div><strong>${(body.history || []).length}</strong><span>Past</span></div></div><p class="eyebrow">References</p><div class="chips">${refs}</div><p class="eyebrow">Open complaints</p><div class="case-list">${current}</div><p class="eyebrow">Past</p>${history}`;
  customers?.querySelectorAll<HTMLButtonElement>("[data-customer]").forEach((button) => {
    button.classList.toggle("active", button.dataset.customer === id);
  });
}

function caseButton(row: CaseItem): string {
  return `<button type="button" class="case-row${row.case_id === selected ? " active" : ""}" data-case="${escapeHtml(row.case_id)}"><strong>${escapeHtml(row.summary)}</strong><small>${statusPill(row)}</small></button>`;
}

function statusPill(row: CaseItem): string {
  const failed = row.release_phase === "reply_failed" || row.release_phase === "handoff_failed";
  const label = failed ? "Delivery failed" : labelStatus(row.status);
  const tone = failed ? "bad" : row.status === "replied" || row.status === "recorded" ? "ok" : row.status === "awaiting_human" ? "wait" : "idle";
  return `<span class="status ${tone}">${escapeHtml(label)}</span>`;
}

function labelStatus(status: string): string {
  if (status === "awaiting_human") return "Decision required";
  if (status === "replied") return "Replied";
  if (status === "recorded") return "Recorded";
  if (status === "in_progress") return "In progress";
  return status.replace(/_/g, " ");
}

async function openCase(id: string) {
  selected = id;
  dirty = false;
  const body = await getJson<{
    detail: {
      case?: { summary?: string; status?: string; customer_id?: string; generation?: number };
      messages?: Array<{ direction: string; body: string }>;
      assignments?: Array<{ role: string; connector_pid: string | null }>;
      actions?: Array<{ effect_type: string; status: string; observed_state: string | null; receipt_id: string | null }>;
    };
    position_draft?: string;
    confirmation?: {
      release_phase?: string;
      confirmed_position?: string;
      last_error?: string | null;
      worker_handoff?: string | null;
    };
  }>(`/api/casepilot/cases/${id}/desk`);
  const detail = body.detail || {};
  const messages = detail.messages || [];
  const worker = (detail.assignments || []).find((row) => row.role === "worker");
  const title = root.querySelector<HTMLElement>("#caseTitle");
  const meta = root.querySelector<HTMLElement>("#caseMeta");
  const badge = root.querySelector<HTMLElement>("#caseStatus");
  if (title) title.textContent = detail.case?.summary || id;
  if (meta) meta.textContent = `${personName(detail.case?.customer_id || customerId)} · ${worker?.connector_pid || "No worker assigned"}`;
  if (badge) {
    const failed = body.confirmation?.release_phase === "reply_failed" || body.confirmation?.release_phase === "handoff_failed";
    badge.textContent = failed ? "Delivery failed" : labelStatus(detail.case?.status || "");
    badge.className = `status ${failed ? "bad" : detail.case?.status === "replied" ? "ok" : detail.case?.status === "awaiting_human" ? "wait" : "idle"}`;
  }
  record?.querySelectorAll<HTMLButtonElement>("[data-case]").forEach((button) => {
    button.classList.toggle("active", button.dataset.case === id);
  });
  const customerThread = root.querySelector<HTMLElement>("#customerThread");
  const workerThread = root.querySelector<HTMLElement>("#workerThread");
  if (customerThread) {
    customerThread.innerHTML = messages
      .filter((row) => row.direction === "customer" || row.direction === "customer_agent" || row.direction === "released")
      .map((row) => bubble(row.direction === "customer" ? "you" : "agent", who(row.direction), row.body))
      .join("") || `<p class="muted">No customer lines yet.</p>`;
    customerThread.scrollTop = customerThread.scrollHeight;
  }
  if (workerThread) {
    const notes = messages.filter((row) => row.direction === "worker_note" || row.direction === "worker_draft");
    workerThread.innerHTML = notes.map((row) => bubble("agent", row.direction === "worker_draft" ? "Draft" : "Worker", row.body)).join("")
      || `<p class="muted">No worker note yet.</p>`;
    workerThread.scrollTop = workerThread.scrollHeight;
  }
  if (position && !dirty) {
    position.value = body.position_draft || body.confirmation?.confirmed_position || "";
  }
  const releasePhase = body.confirmation?.release_phase || "";
  if (phase) {
    phase.textContent = phaseLabel(releasePhase, body.confirmation?.last_error);
  }
  const order = (detail.actions || []).find((row) => row.effect_type === "read_order");
  const refund = (detail.actions || []).find((row) => row.effect_type === "refund");
  if (ops) {
    const observed = order?.observed_state ? "Order result stored" : order ? "Order read was not completed" : "No order read yet";
    const refundLine = refund
      ? `Refund ${escapeHtml(refund.status)}. ${refund.observed_state ? "Observed." : "Not executed."}`
      : "No refund request.";
    ops.innerHTML = `<p>Worker ${worker?.connector_pid ? "on this complaint" : "not assigned"}. ${observed}. ${refundLine}</p><details><summary>Technical detail</summary><pre>${escapeHtml(JSON.stringify({ worker: worker?.connector_pid || null, order, refund, phase: releasePhase }, null, 2))}</pre></details>`;
  }
}

function phaseLabel(value: string, error?: string | null): string {
  if (value === "replied") return "Customer response recorded.";
  if (value === "reply_failed" || value === "handoff_failed") return `Response delivery failed. ${error || "Retry confirm."}`;
  if (value === "reply_pending" || value === "handoff_stored" || value === "position_confirmed") return "Position confirmed. The reply is not stored yet.";
  return "Decision required. Confirm sends the position to the worker, then the customer agent writes the reply.";
}

async function saveDraft() {
  if (!selected || !position) return;
  await postJson(`/api/casepilot/cases/${selected}/position`, { text: position.value });
  dirty = false;
  if (phase) phase.textContent = "Draft saved. It has not been confirmed.";
}

async function confirmPosition() {
  if (!selected || !position) return;
  const text = position.value.trim();
  if (!text) {
    if (phase) phase.textContent = "Write the position before confirming it.";
    return;
  }
  if (phase) phase.textContent = "The worker is handing the position to the customer agent.";
  try {
    const body = await postJson<{ reply: string }>(`/api/casepilot/cases/${selected}/release`, { text });
    dirty = false;
    if (phase) phase.textContent = "Customer response recorded.";
    await openCase(selected);
    if (customerId) await openCustomer(customerId);
    await loadCustomers();
    if (body.reply && phase) phase.textContent = "Customer response recorded.";
  } catch (error) {
    if (phase) phase.textContent = error instanceof Error ? error.message : "The position was kept. The reply was not stored.";
    await openCase(selected);
  }
}

async function readOrder() {
  if (!selected) return;
  const thread = root.querySelector<HTMLElement>("#workerThread");
  thread?.insertAdjacentHTML("beforeend", hint("The worker is asking Connector to read the order."));
  try {
    const body = await postJson<{ read_admitted?: boolean; talk?: { reply?: { text?: string }; error?: string } }>(`/api/casepilot/cases/${selected}/investigate`, {});
    const admitted = body.read_admitted ? "Connector admitted the read." : "Connector did not admit the read.";
    const spoken = body.talk?.reply?.text || body.talk?.error || "";
    thread?.insertAdjacentHTML("beforeend", hint(`${admitted} ${spoken}`));
  } catch (error) {
    thread?.insertAdjacentHTML("beforeend", hint(error instanceof Error ? error.message : "The order read did not finish."));
  }
}

async function act(kind: "cease" | "resume") {
  if (!selected) return;
  const body = await postJson(`/api/casepilot/cases/${selected}/${kind}`, { reason: "supervisor" });
  if (phase) phase.textContent = kind === "cease" ? "This complaint is ceased." : "This complaint is resumed.";
  if (deploy) deploy.textContent = JSON.stringify(body, null, 2);
}

async function loadApprovals() {
  const body = await getJson<{ approvals: Array<{ approval_id: string; action_id: string; scope: string }> }>("/api/casepilot/approvals");
  approvals!.innerHTML = (body.approvals || []).map((row) => `<article class="note"><strong>${escapeHtml(row.scope)}</strong><p>${escapeHtml(row.action_id)}</p><button type="button" data-id="${row.approval_id}" data-act="approve">Approve</button><button type="button" class="ghost" data-id="${row.approval_id}" data-act="deny">Deny</button></article>`).join("")
    || `<p class="muted">No money approval is waiting.</p>`;
}

async function loadCatalog() {
  const form = root.querySelector<HTMLFormElement>("#catalog");
  if (!form) return;
  const body = await getJson<{ catalog: Array<{ id: string; label: string; enabled: boolean }> }>("/api/casepilot/catalog/manage");
  form.innerHTML = `${(body.catalog || []).map((item) => `<label class="check"><input type="checkbox" value="${item.id}" ${item.enabled ? "checked" : ""}> ${escapeHtml(item.label)}</label>`).join("")}<button type="submit">Store in Connector knowledge</button>`;
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const ids = [...form.querySelectorAll<HTMLInputElement>("input:checked")].map((input) => input.value);
    const saved = await postJson("/api/casepilot/catalog", { ids });
    if (deploy) deploy.textContent = JSON.stringify(saved, null, 2);
  });
}

function who(direction: string): string {
  if (direction === "customer") return "Customer";
  if (direction === "customer_agent") return "Customer agent";
  if (direction === "released") return "Sent";
  return direction;
}

function bubble(kind: string, label: string, body: string): string {
  return `<article class="bubble ${kind}"><span>${escapeHtml(label)}</span><p>${escapeHtml(body)}</p></article>`;
}

function hint(text: string): string {
  return `<p class="chat-hint">${escapeHtml(text)}</p>`;
}

function personName(id: string): string {
  const bare = id.replace(/^cust_/, "").replace(/[._]+/g, " ").trim();
  return bare.replace(/\b[a-z]/g, (letter) => letter.toUpperCase()) || id;
}

function initials(id: string): string {
  const parts = personName(id).split(" ").filter(Boolean);
  return ((parts[0]?.[0] || "") + (parts[1]?.[0] || "")).toUpperCase() || "C";
}

function when(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value.slice(0, 16);
  return date.toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>]/g, (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[char] || char);
}

window.setInterval(() => {
  if (signedIn) void loadCustomers();
}, 15000);
