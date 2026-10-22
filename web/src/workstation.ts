import { getJson, postJson, signIn, useSurface } from "./api.js";

useSurface("desk");

void getJson<{ account?: string | null; ready: boolean }>("/api/casepilot/setup/status").then((gate) => {
  if (!gate.account || !gate.ready) window.location.replace("/");
});

const root = document.querySelector<HTMLElement>("#app") ?? missing();
function missing(): never {
  throw new Error("missing app");
}

let selected = "";
let signedIn = false;

root.innerHTML = `
  <header class="top">
    <div class="brand">CasePilot<small>Desk</small></div>
    <nav class="links"><a href="/">Open</a><a href="/portal">Report</a><a href="/supervisor">Review</a></nav>
    <div class="actions">
      <button type="button" id="cease" class="ghost">Cease</button>
      <button type="button" id="confirm" class="ghost">Confirm</button>
      <button type="button" id="approve">Release</button>
    </div>
  </header>
  <main class="desk">
    <section class="card" id="gate">
      <p class="eyebrow">Sign in</p>
      <form id="who">
        <label>Caseworker <input name="principal" value="operator.nolan" required></label>
        <button type="submit">Open the desk</button>
      </form>
    </section>
    <section class="card">
      <p class="eyebrow">Reports</p>
      <div id="queue"></div>
    </section>
    <section class="card">
      <p class="eyebrow">Notifications</p>
      <div id="notes"></div>
    </section>
    <section class="card">
      <p class="eyebrow">Label</p>
      <div class="labels" id="labels">
        <span data-label="easy">Easy</span>
        <span data-label="augmented">Tough</span>
        <span data-label="hard">Hard</span>
      </div>
      <label>Decision
        <select id="choice">
          <option value="replacement">Replacement</option>
          <option value="refund">Refund</option>
          <option value="decline">Decline</option>
          <option value="need_more">Ask for more</option>
        </select>
      </label>
      <label>Reply to release <textarea id="note" placeholder="The worker draft appears here. Edit it, then release."></textarea></label>
      <form id="find">
        <input name="q" placeholder="Search kept words">
        <button type="submit">Search</button>
      </form>
      <div id="hits"></div>
    </section>
    <section class="card chat-card">
      <p class="eyebrow">Agent on this file</p>
      <div id="chat" class="thread"></div>
      <form id="chatForm">
        <input name="message" placeholder="Ask about this complaint" required>
        <button type="submit">Send</button>
      </form>
    </section>
    <section class="card">
      <p class="eyebrow">Filesystem</p>
      <pre id="file">No case file yet.</pre>
    </section>
    <section class="card wide">
      <p class="eyebrow">Custody</p>
      <div id="custody" class="muted">Open a case to walk the graph from the words to the receipt.</div>
    </section>
    <section class="card wide">
      <p class="eyebrow">Verification</p>
      <div id="verification" class="muted">Open a case to read what the record actually meets.</div>
    </section>
    <section class="card">
      <p class="eyebrow">Research</p>
      <div id="research" class="thread"></div>
    </section>
  </main>
`;

const notes = root.querySelector<HTMLElement>("#notes");
const queue = root.querySelector<HTMLElement>("#queue");
const chat = root.querySelector<HTMLElement>("#chat");
const research = root.querySelector<HTMLElement>("#research");
const file = root.querySelector<HTMLElement>("#file");
const labels = root.querySelector<HTMLElement>("#labels");

root.querySelector<HTMLFormElement>("#who")?.addEventListener("submit", async (event) => {
  event.preventDefault();
  const data = new FormData(event.target as HTMLFormElement);
  await openDesk(String(data.get("principal") || "operator.nolan"));
});

async function openDesk(principal: string) {
  const ready = await getJson<{ ready: boolean; reason?: string }>("/api/casepilot/setup/status");
  if (!ready.ready) {
    queue!.innerHTML = `<p class="note">${escapeHtml(ready.reason || "Connector is not live. The desk stays closed.")}</p>`;
    return;
  }
  await signIn("operator", principal, "desk");
  signedIn = true;
  await loadQueue();
  await loadNotes();
}

void openDesk("operator.nolan");

root.querySelector("#cease")?.addEventListener("click", async () => {
  if (!selected) return;
  const body = await postJson(`/api/casepilot/cases/${selected}/cease`, { reason: "operator cease" });
  file!.textContent = JSON.stringify(body, null, 2);
});

root.querySelector("#confirm")?.addEventListener("click", async () => {
  if (!selected) return;
  const body = await postJson(`/api/casepilot/cases/${selected}/confirm`, {});
  await openCase(selected);
  file!.textContent = JSON.stringify(body, null, 2);
});

root.querySelector("#approve")?.addEventListener("click", async () => {
  if (!selected) return;
  const choice = root.querySelector<HTMLSelectElement>("#choice")?.value || "decline";
  const note = root.querySelector<HTMLTextAreaElement>("#note")?.value || "";
  const body = choice === "decline" || choice === "need_more"
    ? await postJson(`/api/casepilot/cases/${selected}/release`, { text: note })
    : await postJson(`/api/casepilot/cases/${selected}/decide`, { choice, note });
  await openCase(selected);
  file!.textContent = JSON.stringify(body, null, 2);
});

root.querySelector<HTMLFormElement>("#chatForm")?.addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = event.target as HTMLFormElement;
  const data = new FormData(form);
  const message = String(data.get("message") || "").trim();
  if (!message) return;
  if (!selected) {
    chat!.insertAdjacentHTML("beforeend", `<p class="note">Open a report first. The agent answers on that file.</p>`);
    return;
  }
  chat!.insertAdjacentHTML("beforeend", `<p class="note">The agent is reading this.</p>`);
  try {
    const body = await postJson<{ reply: string }>(`/api/casepilot/cases/${selected}/chat`, { message });
    form.reset();
    chat!.insertAdjacentHTML("beforeend", `<p><strong>You</strong> ${escapeHtml(message)}</p><p><strong>Agent</strong> ${escapeHtml(body.reply)}</p>`);
    await openCase(selected);
  } catch (error) {
    chat!.insertAdjacentHTML(
      "beforeend",
      `<p class="note">${escapeHtml(error instanceof Error ? error.message : "The agent did not answer.")}</p>`,
    );
  }
});

function openFrom(event: Event) {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-case]");
  if (!button?.dataset.case) return;
  selected = button.dataset.case;
  void openCase(selected);
}

notes?.addEventListener("click", openFrom);
queue?.addEventListener("click", openFrom);

async function loadQueue() {
  try {
    const body = await getJson<{ cases: Array<{ case_id: string; summary: string; status: string; customer_id: string }> }>("/api/casepilot/work/queue");
    queue!.innerHTML = (body.cases || [])
      .map((row) => `<button type="button" class="note" data-case="${row.case_id}"><strong>${escapeHtml(row.status)}</strong> ${escapeHtml(row.customer_id)} — ${escapeHtml(row.summary)}</button>`)
      .join("") || `<p class="muted">No reports are stored.</p>`;
  } catch (error) {
    queue!.innerHTML = `<p class="note">${escapeHtml(error instanceof Error ? error.message : "The desk could not read the reports.")}</p>`;
  }
}

async function loadNotes() {
  const body = await getJson<{ notifications: Array<{ case_id: string; message: string }> }>("/api/casepilot/work/notifications");
  notes!.innerHTML = body.notifications
    .map((row) => `<button type="button" class="note" data-case="${row.case_id}">${escapeHtml(row.message)}</button>`)
    .join("") || `<p class="muted">No notifications.</p>`;
}

root.querySelector<HTMLFormElement>("#find")?.addEventListener("submit", async (event) => {
  event.preventDefault();
  const data = new FormData(event.target as HTMLFormElement);
  const body = await getJson<{ present?: boolean; reason?: string; hits?: unknown }>(
    `/api/casepilot/library/search?q=${encodeURIComponent(String(data.get("q") || ""))}`,
  );
  const hits = root.querySelector<HTMLElement>("#hits");
  if (hits) hits.textContent = JSON.stringify(body.hits || body.reason || body, null, 2);
});

async function openCase(id: string) {
  const body = await getJson<{
    detail: { triage: { classification?: string } | null; messages?: Array<{ direction: string; body: string }> };
    filesystem: { present?: boolean; path?: string; body?: string; error?: string };
    research: Array<{ body?: string; created_at?: string }>;
  }>(`/api/casepilot/cases/${id}/desk`);
  const messages = body.detail.messages || [];
  const draft = [...messages].reverse().find((row) => row.direction === "worker_draft");
  const note = root.querySelector<HTMLTextAreaElement>("#note");
  if (note && draft?.body) note.value = draft.body;
  if (chat) {
    chat.innerHTML = messages
      .filter((row) => row.direction !== "worker_draft" && row.direction !== "worker_note")
      .map((row) => `<p><strong>${escapeHtml(row.direction)}</strong> ${escapeHtml(row.body)}</p>`)
      .join("") || `<p class="muted">No kept lines yet.</p>`;
  }
  const classification = body.detail.triage?.classification || "";
  labels?.querySelectorAll("[data-label]").forEach((node) => {
    node.classList.toggle("on", node.getAttribute("data-label") === classification);
  });
  const stored = body.filesystem;
  file!.textContent = stored.present ? `${stored.path || ""}\n\n${stored.body || ""}` : stored.error || "No file stored yet.";
  research!.innerHTML = body.research
    .map((row) => `<article><time>${row.created_at || ""}</time><pre>${escapeHtml(row.body || "")}</pre></article>`)
    .join("") || `<p class="muted">No stored research yet.</p>`;
  const custody = root.querySelector<HTMLElement>("#custody");
  const graph = (body as { custody?: { present?: boolean; reason?: string; gate?: string; effects_sealed?: number; effects_refused?: number; chain?: Array<{ step: string; who: string; text: string }> } }).custody;
  if (custody) {
    if (!graph?.present) {
      custody.innerHTML = `<p>${escapeHtml(graph?.reason || "The custody graph is not sealed.")}</p>`;
    } else {
      const steps = (graph.chain || [])
        .map((step) => `<li><strong>${escapeHtml(step.step)}</strong> ${escapeHtml(step.who)} — ${escapeHtml(step.text)}</li>`)
        .join("");
      custody.innerHTML = `<p class="pill">${graph.effects_sealed || 0} effects sealed · ${graph.effects_refused || 0} not proceed</p><p class="muted">${escapeHtml(graph.gate || "")}</p><ol class="chain">${steps}</ol>`;
    }
  }
  const verification = root.querySelector<HTMLElement>("#verification");
  const record = (body as { verification?: { satisfied?: boolean; sections?: Array<{ title: string; items: Array<{ name: string; met: boolean }> }> } }).verification;
  if (verification && record?.sections) {
    verification.innerHTML = `<p class="pill">${record.satisfied ? "Record satisfied" : "Record open"}</p>` + record.sections
      .map((section) => `<article><strong>${escapeHtml(section.title)}</strong><ul>${section.items.map((item) => `<li>${item.met ? "Met" : "Open"} — ${escapeHtml(item.name)}</li>`).join("")}</ul></article>`)
      .join("");
  }
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>]/g, (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[char] || char);
}

window.setInterval(() => {
  if (signedIn) {
    void loadQueue();
    void loadNotes();
  }
}, 5000);
