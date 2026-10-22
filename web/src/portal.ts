import { getJson, postJson, signIn, useSurface } from "./api.js";

useSurface("report");

void getJson<{ account?: string | null; ready: boolean }>("/api/casepilot/setup/status").then((gate) => {
  if (!gate.account || !gate.ready) window.location.replace("/");
});

const root = document.querySelector<HTMLElement>("#app");
if (!root) throw new Error("missing app");

type Chip = { id: string; label: string; group: string; prompt: string };

root.innerHTML = `
  <header class="top">
    <div class="brand">CasePilot<small>Report</small></div>
    <nav class="links"><a href="/workstation">Desk</a><a href="/supervisor">Review</a></nav>
  </header>
  <main class="report">
    <aside class="card report-side">
      <form id="who">
        <label>Your name <input name="name" required placeholder="The name on this report"></label>
        <label>Reference you already have <input name="subject" placeholder="Order, bill, or visit. Leave blank if you have none."></label>
        <button type="submit">Open your reports</button>
      </form>
      <p class="eyebrow">Your reports</p>
      <div id="shelf" class="report-list"></div>
    </aside>
    <section class="card report-chat">
      <header class="chat-head">
        <p class="eyebrow">Conversation</p>
        <p id="chatTitle" class="muted">Type what happened. The reply appears in this pane.</p>
      </header>
      <div id="thread" class="thread"></div>
      <div class="chat-tools">
        <div id="tabs" class="tabs"></div>
        <div id="chips" class="chips"></div>
      </div>
      <form id="composer" hidden>
        <input name="message" placeholder="What happened">
        <button type="submit">Send</button>
      </form>
    </section>
  </main>
`;

const shelf = root.querySelector<HTMLElement>("#shelf");
const thread = root.querySelector<HTMLElement>("#thread");
const chatTitle = root.querySelector<HTMLElement>("#chatTitle");
const tabs = root.querySelector<HTMLElement>("#tabs");
const chips = root.querySelector<HTMLElement>("#chips");
const composer = root.querySelector<HTMLFormElement>("#composer");

let group = "";
let types: Chip[] = [];
let activeCase = "";
let chosen: Chip | null = null;
let signedIn = false;
let subject = "";

void open();

root.querySelector<HTMLFormElement>("#who")?.addEventListener("submit", async (event) => {
  event.preventDefault();
  const data = new FormData(event.target as HTMLFormElement);
  const name = String(data.get("name") || "").trim();
  subject = String(data.get("subject") || "").trim();
  const slug = name.toLowerCase().replace(/[^a-z0-9]+/g, ".").replace(/^\.|\.$/g, "") || "customer";
  const id = `cust_${slug.replace(/\./g, "_")}`;
  await signIn("customer", slug, id);
  signedIn = true;
  composer!.hidden = false;
  await loadShelf();
});

async function open() {
  const status = await getJson<{ reachable: boolean; ready: boolean; reason: string }>("/api/casepilot/setup/status");
  if (!status.reachable || !status.ready) {
    thread!.innerHTML = `<p class="note">${status.reason}</p>`;
    composer!.hidden = true;
    return;
  }
  let catalog: { knowledge_stored: boolean; types: Chip[]; honesty: string };
  try {
    catalog = await getJson("/api/casepilot/catalog");
  } catch (error) {
    thread!.textContent = error instanceof Error ? error.message : "Connector refused the catalog.";
    composer!.hidden = true;
    return;
  }
  types = catalog.knowledge_stored ? catalog.types : [];
  const groups = [...new Set(types.map((item) => item.group))];
  group = groups[0] || "";
  tabs!.innerHTML = groups.map((name) => `<button type="button" data-group="${name}">${labelGroup(name)}</button>`).join("");
  paintChips();
  composer!.hidden = false;
  try {
    const session = await getJson<{ role: string; principal: string }>("/api/casepilot/session");
    if (session.role === "customer" && session.principal) {
      signedIn = true;
      const name = root!.querySelector<HTMLInputElement>("#who input[name=name]");
      if (name && !name.value) name.value = session.principal.replace(/\./g, " ");
      await loadShelf();
      const latest = shelf?.querySelector<HTMLButtonElement>("[data-case]");
      if (latest?.dataset.case) {
        activeCase = latest.dataset.case;
        await showCase(activeCase);
        return;
      }
    }
  } catch {
    signedIn = false;
  }
  thread!.innerHTML = `<p class="muted">Enter your name, type what happened, and Send. Choose a case type when you know it. The agent answers from Connector.</p>`;
}

async function loadShelf() {
  const body = await getJson<{ cases: Array<{ case_id: string; summary: string; status: string }> }>("/api/casepilot/cases");
  shelf!.innerHTML = (body.cases || [])
    .map((row) => `<button type="button" data-case="${row.case_id}" class="${row.case_id === activeCase ? "active" : ""}"><strong>${escapeHtml(row.summary)}</strong><small>${escapeHtml(row.status)}</small></button>`)
    .join("") || `<p class="muted">No reports yet.</p>`;
}

tabs?.addEventListener("click", (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-group]");
  if (!button?.dataset.group) return;
  group = button.dataset.group;
  paintChips();
});

shelf?.addEventListener("click", (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-case]");
  if (!button?.dataset.case) return;
  activeCase = button.dataset.case;
  void showCase(activeCase);
});

chips?.addEventListener("click", async (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-type]");
  if (!button?.dataset.type) return;
  if (!(await ensurePerson())) return;
  const chip = types.find((item) => item.id === button.dataset.type);
  if (!chip) return;
  chosen = chip;
  activeCase = "";
  paintChips();
  const pending = String(new FormData(composer!).get("message") || "").trim();
  if (pending) {
    await file(chip.id, pending);
    return;
  }
  if (chatTitle) chatTitle.textContent = chip.label;
  thread!.innerHTML = hint(`${chip.label}. ${chip.prompt} Type what happened, then Send.`);
  composer?.querySelector("input")?.focus();
});

composer?.addEventListener("submit", async (event) => {
  event.preventDefault();
  const data = new FormData(composer!);
  const message = String(data.get("message") || "").trim();
  if (!message) {
    if (activeCase) await showCase(activeCase);
    else thread!.insertAdjacentHTML("beforeend", hint("Type what happened, then Send."));
    return;
  }
  if (!(await ensurePerson())) return;
  if (activeCase) {
    thread!.insertAdjacentHTML("beforeend", hint("The agent is reading this message."));
    try {
      await postJson(`/api/casepilot/cases/${activeCase}/info`, { detail: message });
      composer!.reset();
    } catch (error) {
      thread!.insertAdjacentHTML("beforeend", hint(error instanceof Error ? error.message : "The agent did not answer."));
    }
    await showCase(activeCase);
    return;
  }
  if (!chosen) {
    if (chatTitle) chatTitle.textContent = "Before a case type";
    thread!.innerHTML = hint("The agent is reading this.");
    try {
      const body = await postJson<{ text: string; case?: { case_id: string } }>("/api/casepilot/catalog/ask", {
        message,
        subject_ref: subject,
      });
      composer!.reset();
      if (body.case?.case_id) {
        activeCase = body.case.case_id;
        chosen = null;
        await loadShelf();
        await showCase(activeCase);
        return;
      }
      thread!.innerHTML = bubble("customer", message) + bubble("customer_agent", body.text);
      stick();
    } catch (error) {
      thread!.innerHTML = hint(error instanceof Error ? error.message : "The agent did not answer.");
    }
    return;
  }
  await file(chosen.id, message);
});

async function ensurePerson(): Promise<boolean> {
  if (signedIn) return true;
  const who = root!.querySelector<HTMLFormElement>("#who");
  const name = String(new FormData(who!).get("name") || "").trim();
  subject = String(new FormData(who!).get("subject") || "").trim();
  if (!name) {
    thread!.insertAdjacentHTML("beforeend", hint("Enter your name above, then Send. The agent answers after that."));
    who?.querySelector<HTMLInputElement>("input[name=name]")?.focus();
    return false;
  }
  const slug = name.toLowerCase().replace(/[^a-z0-9]+/g, ".").replace(/^\.|\.$/g, "") || "customer";
  const id = `cust_${slug.replace(/\./g, "_")}`;
  try {
    await signIn("customer", slug, id);
  } catch (error) {
    thread!.insertAdjacentHTML("beforeend", hint(error instanceof Error ? error.message : "Sign-in failed."));
    return false;
  }
  signedIn = true;
  await loadShelf();
  return true;
}

function paintChips() {
  chips!.innerHTML = types
    .filter((item) => item.group === group)
    .map((item) => `<button type="button" class="kind${chosen?.id === item.id ? " active" : ""}" data-type="${item.id}">${item.label}</button>`)
    .join("");
  tabs?.querySelectorAll("button").forEach((node) => {
    node.classList.toggle("active", node.getAttribute("data-group") === group);
  });
}

async function file(typeId: string, summary: string) {
  thread!.innerHTML = hint("The agent is reading this against the stored case type.");
  try {
    const created = await postJson<{ case: { case_id: string }; honesty?: string; classification_error?: string; triage?: { receipt?: string } }>(
      "/api/casepilot/cases",
      { issue_type: typeId, summary, subject_ref: subject },
    );
    activeCase = created.case.case_id;
    chosen = null;
    paintChips();
    composer!.reset();
    await loadShelf();
    await showCase(activeCase);
    if (created.honesty) thread?.insertAdjacentHTML("beforeend", hint(created.honesty));
    if (created.classification_error) thread?.insertAdjacentHTML("beforeend", hint(created.classification_error));
  } catch (error) {
    thread?.insertAdjacentHTML("beforeend", hint(error instanceof Error ? error.message : "The report was not filed."));
  }
}

async function showCase(id: string) {
  const detail = await getJson<{
    case: { case_id: string; summary: string; status: string };
    messages?: Array<{ direction: string; body: string }>;
  }>(`/api/casepilot/cases/${id}`);
  const lines = (detail.messages || []).filter((row) => row.direction !== "worker_draft" && row.direction !== "worker_note");
  if (chatTitle) chatTitle.textContent = detail.case.summary;
  thread!.innerHTML = lines.map((row) => bubble(row.direction, row.body)).join("")
    || hint("This report is kept. A reply appears here when it has been released.");
  shelf?.querySelectorAll<HTMLButtonElement>("[data-case]").forEach((button) => {
    button.classList.toggle("active", button.dataset.case === id);
  });
  stick();
}

function bubble(direction: string, body: string): string {
  const kind = direction === "customer" ? "you" : direction === "released" ? "desk" : "agent";
  return `<article class="bubble ${kind}"><span>${escapeHtml(who(direction))}</span><p>${escapeHtml(body)}</p></article>`;
}

function hint(text: string): string {
  return `<p class="chat-hint">${escapeHtml(text)}</p>`;
}

window.setInterval(() => {
  if (signedIn && activeCase) void showCase(activeCase);
}, 4000);

function stick() {
  if (!thread) return;
  thread.scrollTop = thread.scrollHeight;
}

function who(direction: string): string {
  if (direction === "customer") return "You";
  if (direction === "released") return "Desk";
  if (direction === "customer_agent" || direction === "receipt") return "Agent";
  return direction;
}

function labelGroup(value: string): string {
  if (value === "public_safety") return "Public safety";
  if (value === "business") return "Business";
  if (value === "care") return "Care and facility";
  return value;
}

function labelName(value: string): string {
  if (value === "easy") return "Easy";
  if (value === "hard") return "Hard";
  if (value === "augmented") return "Tough";
  return "Unlabeled";
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>]/g, (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[char] || char);
}
