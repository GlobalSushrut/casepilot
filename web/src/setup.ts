import { getJson, postJson } from "./api.js";

const root = document.querySelector<HTMLElement>("#gate") ?? missing();
function missing(): never {
  throw new Error("missing app");
}

type Provider = {
  id: string;
  label: string;
  needs_key: boolean;
  needs_endpoint: boolean;
  default_endpoint?: string;
  models: string[];
};

type Status = {
  ready: boolean;
  reason: string;
  authenticated: boolean;
  connector_real: boolean;
  account?: string | null;
  llm_mode: string;
  desk_mode: string;
  model: string;
  catalog?: Provider[];
  llm?: { provider?: string | null; model?: string | null };
};

const fallback: Provider[] = [
  { id: "openai", label: "OpenAI", needs_key: true, needs_endpoint: false, models: ["gpt-4o", "gpt-4o-mini", "gpt-4.1", "o4-mini"] },
  { id: "anthropic", label: "Anthropic", needs_key: true, needs_endpoint: false, models: ["claude-3-5-sonnet-20241022", "claude-3-5-haiku-20241022", "claude-3-opus-20240229"] },
  { id: "gemini", label: "Google Gemini", needs_key: true, needs_endpoint: false, models: ["gemini-2.0-flash", "gemini-1.5-pro"] },
  { id: "deepseek", label: "DeepSeek", needs_key: true, needs_endpoint: false, models: ["deepseek-chat", "deepseek-reasoner"] },
  { id: "groq", label: "Groq", needs_key: true, needs_endpoint: false, models: ["llama-3.3-70b-versatile", "mixtral-8x7b-32768"] },
  { id: "together", label: "Together", needs_key: true, needs_endpoint: false, models: ["meta-llama/Llama-3-70b-chat-hf"] },
  { id: "mistral", label: "Mistral", needs_key: true, needs_endpoint: false, models: ["mistral-large-latest", "mistral-small-latest"] },
  { id: "cohere", label: "Cohere", needs_key: true, needs_endpoint: false, models: ["command-r-plus"] },
  { id: "fireworks", label: "Fireworks", needs_key: true, needs_endpoint: false, models: ["accounts/fireworks/models/llama-v3p1-70b-instruct"] },
  { id: "perplexity", label: "Perplexity", needs_key: true, needs_endpoint: false, models: ["sonar"] },
  { id: "openrouter", label: "OpenRouter", needs_key: true, needs_endpoint: false, models: ["openai/gpt-4o", "anthropic/claude-3.5-sonnet", "meta-llama/llama-3.1-70b-instruct"] },
  { id: "ollama", label: "Ollama (local)", needs_key: false, needs_endpoint: true, default_endpoint: "http://127.0.0.1:11434/v1", models: ["llama3.2", "llama3.1", "mistral"] },
  { id: "lmstudio", label: "LM Studio (local)", needs_key: false, needs_endpoint: true, default_endpoint: "http://127.0.0.1:1234/v1", models: ["local-model"] },
  { id: "vllm", label: "vLLM (self-host)", needs_key: false, needs_endpoint: true, default_endpoint: "http://127.0.0.1:8000/v1", models: ["local-model"] },
  { id: "custom", label: "Custom OpenAI-compatible API", needs_key: false, needs_endpoint: true, models: [] },
];

function esc(value: string): string {
  return value.replace(/[&<>"']/g, (ch) => {
    if (ch === "&") return "&amp;";
    if (ch === "<") return "&lt;";
    if (ch === ">") return "&gt;";
    if (ch === '"') return "&quot;";
    return "&#39;";
  });
}

function catalogOf(status: Status): Provider[] {
  const rows = Array.isArray(status.catalog) ? status.catalog.filter((row) => row && row.id) : [];
  return rows.length > 0 ? rows : fallback;
}

function keyOptional(provider: Provider): boolean {
  return !provider.needs_key || provider.id === "custom" || provider.id === "ollama" || provider.id === "lmstudio" || provider.id === "vllm";
}

function linkForm(providers: Provider[], selected: string, model: string): string {
  const options = providers
    .map((row) => `<option value="${esc(row.id)}"${row.id === selected ? " selected" : ""}>${esc(row.label || row.id)}</option>`)
    .join("");
  const current = providers.find((row) => row.id === selected) ?? providers[0];
  const suggestions = (current?.models ?? []).map((name) => `<option value="${esc(name)}"></option>`).join("");
  const endpoint = current?.needs_endpoint ? current.default_endpoint || "" : "";
  return `
    <form id="link">
      <label>Provider
        <select name="provider" required>${options}</select>
      </label>
      <label>Model
        <input name="model" list="models" value="${esc(model || current?.models[0] || "")}" required placeholder="Type the model id this vendor serves">
        <datalist id="models">${suggestions}</datalist>
      </label>
      <label>Vendor API key
        <input name="api_key" type="password" autocomplete="off" placeholder="Required for a hosted vendor">
      </label>
      <label id="endpoint-label">Endpoint
        <input name="endpoint" value="${esc(endpoint)}" placeholder="OpenAI-compatible base URL">
      </label>
      <button type="submit">Link the model on Connector</button>
      <p id="link-note" class="muted">The key is sent to Connector and is not stored here. Type any model id the vendor currently serves. A vendor that is not listed uses Custom and its base URL. A local endpoint must be reachable from the Connector service.</p>
    </form>
  `;
}

function applyProvider(form: HTMLFormElement, providers: Provider[]): void {
  const id = String(new FormData(form).get("provider") || "");
  const provider = providers.find((row) => row.id === id) ?? providers[0];
  if (!provider) return;
  const model = form.querySelector<HTMLInputElement>('input[name="model"]');
  const key = form.querySelector<HTMLInputElement>('input[name="api_key"]');
  const endpoint = form.querySelector<HTMLInputElement>('input[name="endpoint"]');
  const endpointLabel = form.querySelector<HTMLElement>("#endpoint-label");
  const list = form.querySelector("#models");
  if (list) {
    list.innerHTML = provider.models.map((name) => `<option value="${esc(name)}"></option>`).join("");
  }
  if (model && !provider.models.includes(model.value)) {
    model.value = provider.models[0] || "";
  }
  if (key) key.required = !keyOptional(provider);
  if (endpoint && endpointLabel) {
    endpointLabel.hidden = !provider.needs_endpoint;
    endpoint.required = provider.needs_endpoint;
    if (provider.needs_endpoint && !endpoint.value) endpoint.value = provider.default_endpoint || "";
    if (!provider.needs_endpoint) endpoint.value = "";
  }
}

function emailForm(value: string): string {
  return `
    <form id="email">
      <label>Email
        <input name="email" type="email" required autocomplete="email" value="${esc(value)}" placeholder="you@company.com">
      </label>
      <button type="submit">Continue</button>
      <p id="email-note" class="muted">This email is the trial. Another email cannot see its complaints, customers, or model.</p>
    </form>
  `;
}

function wireEmail(): void {
  const form = root.querySelector<HTMLFormElement>("#email");
  if (!form) return;
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const note = root.querySelector("#email-note");
    const email = String(new FormData(form).get("email") || "").trim();
    if (note) note.textContent = "Opening this trial…";
    try {
      await postJson("/api/casepilot/setup/account", { email });
      await paint();
    } catch (error) {
      if (note) note.textContent = error instanceof Error ? error.message : "That email was refused.";
    }
  });
}

function wire(providers: Provider[]): void {
  const form = root.querySelector<HTMLFormElement>("#link");
  if (!form) return;
  applyProvider(form, providers);
  form.querySelector('select[name="provider"]')?.addEventListener("change", () => applyProvider(form, providers));
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const data = new FormData(form);
    const endpoint = String(data.get("endpoint") || "").trim();
    const note = root.querySelector("#link-note");
    if (note) note.textContent = "Linking on Connector…";
    try {
      await postJson("/api/casepilot/setup/llm", {
        provider: String(data.get("provider") || ""),
        model: String(data.get("model") || ""),
        api_key: String(data.get("api_key") || ""),
        endpoint: endpoint || null,
      });
      await paint();
      return;
    } catch (error) {
      if (note) note.textContent = error instanceof Error ? error.message : "The model link was refused.";
    }
  });
}

let wait: number | undefined;

async function paint(): Promise<void> {
  if (wait !== undefined) {
    window.clearTimeout(wait);
    wait = undefined;
  }
  const status = await getJson<Status>("/api/casepilot/setup/status");
  if (root.contains(document.activeElement) && (root.querySelector("#link") || root.querySelector("#email"))) {
    if (!status.authenticated) wait = window.setTimeout(() => void paint(), 4000);
    return;
  }
  const providers = catalogOf(status);
  const linked = status.llm?.provider || "openai";
  const model = status.llm?.model || status.model || "";
  const account = status.account || "";
  if (!account) {
    root.innerHTML = `
      <p class="eyebrow">Your email</p>
      <h2>Start a separate trial</h2>
      <p>${esc(status.reason)}</p>
      ${emailForm("")}
    `;
    wireEmail();
    return;
  }
  const doors = status.ready
    ? `<nav class="doors">
        <a href="/portal">Report<small>File a complaint as the customer</small></a>
        <a href="/workstation">Desk<small>Work the case as the company</small></a>
        <a href="/supervisor">Review<small>Confirm the position people will hear</small></a>
      </nav>`
    : `<nav class="doors waiting">
        <span>Report<small>Opens after a model is linked</small></span>
        <span>Desk<small>Opens after a model is linked</small></span>
        <span>Review<small>Opens after a model is linked</small></span>
      </nav>`;
  if (status.ready) {
    root.innerHTML = `
      <p class="eyebrow">Try it</p>
      <h2>The rooms are open</h2>
      <p>${esc(account)}</p>
      <p>${esc(status.reason)}</p>
      <p class="pill">Connector live · ${esc(linked)} · ${esc(model || status.desk_mode || status.llm_mode)}</p>
      ${doors}
      <details>
        <summary>Use a different model</summary>
        ${linkForm(providers, linked, model)}
      </details>
      ${emailForm(account)}
    `;
    wire(providers);
    wireEmail();
    return;
  }
  root.innerHTML = `
    <p class="eyebrow">Your model</p>
    <h2>Link the model for ${esc(account)}</h2>
    <p>${esc(status.reason)}</p>
    ${doors}
    ${linkForm(providers, linked || "openai", model || "")}
    ${emailForm(account)}
  `;
  wire(providers);
  wireEmail();
  if (!status.authenticated) wait = window.setTimeout(() => void paint(), 4000);
}

void paint();
