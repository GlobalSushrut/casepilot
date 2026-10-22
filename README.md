# CasePilot

[![Deploy to Render](https://render.com/images/deploy-to-render-button.svg)](https://render.com/deploy?repo=https://github.com/GlobalSushrut/casepilot)

CasePilot is a customer-case application. A customer reports a problem, a caseworker investigates it, a policy recommendation is recorded, and a permitted remedy can be carried out. The remedy in this build is a replacement, or a simulated refund when the amount is above the automatic limit and a supervisor approves it.

CasePilot keeps the business record: the case, the issues, the evidence digests, the plan, the intended action, the approval, and the observed result. Connector OS keeps the agent work: identity, memory, the model call, admission of each effect, and the ability to cease an agent.

CasePilot does not contain Connector source. It talks to an installed Connector over HTTP.

## What you can do with it

Three people use it.

- A customer opens the portal, signs in, and submits a complaint. They can read the case status and whether an outcome was verified. They cannot approve, cease, or refund.
- A caseworker opens the workstation, investigates, asks for a policy recommendation, and proposes a replacement or a refund. The screen shows `admitted`, `denied`, or `approval_required`. A model paragraph on that screen is not a completed refund.
- A supervisor opens the console, approves or denies a pending refund, ceases the case agents, or resumes the case on a new generation. The console also shows whether Connector itself is up.

The useful place for this is a service desk that must let agents look at orders and recommend a remedy, while a refund or replacement still has to pass an authority that the agent does not control. A retail returns desk, a warranty desk, or an internal support queue are the shape of that job. This repository ships one synthetic order so that path can be exercised on one machine.

## Why Connector is in the path

Before Connector, the usual way to build this was to put a model and a set of tools in the application. The same process that wrote the recommendation also held the API credential that created the refund. A prompt could tell the model not to refund above a limit. Nothing outside the model enforced that. An approval stored in the app could be treated as a permanent permission. Stopping the agent meant hoping the next model call cooperated. Logs could say a refund was intended. They could not show that this agent was admitted to do it and that the business row then changed.

Connector is the place that owns those controls:

| CasePilot needs | Connector provides | What CasePilot does with the answer |
| --- | --- | --- |
| Three named agents for one case | `POST /api/v1/agents` with `name`, `namespace`, `role`, `purpose`, `instructions`, and `model` | Stores the returned intelligence id. It does not invent a second agent runtime. |
| A model answer bound to that agent | `POST /api/v1/agents/:pid/completions` with `model`, `agent_pid`, and `messages` | Shows the text as a recommendation. It does not treat the text as permission. |
| An outside agent, if you already have one | Connector's OpenAI-compatible gateway at `/v1/chat/completions` | Tells you the gateway URL and the agent id. The vendor key stays on Connector. |
| Case memory | `POST /api/v1/memory/write` | Writes the complaint onto the investigator. The order and the case status stay in CasePilot's database. |
| A replacement or refund | `POST /api/v1/tools/mcp/invoke` with `bridge_id`, `tool`, `agent_pid`, `input`, and `idempotency_key` | Writes the business row only when the outcome is `Success` or `proceed`. |
| A refund above $50 | Connector may ask. CasePilot also keeps its own approval. | The supervisor approves a scope and a 15-minute expiry, then CasePilot submits the same effect again. |
| Stop | `POST /api/v1/agents/:pid/cease` | Blocks further effects until resume opens a new generation. |

The model vendor key is linked on Connector (`POST /api/v1/settings/llms/link` with `provider`, `model`, and `api_key`). CasePilot never stores that key and never calls the vendor. If Connector reports the model lane as `disconnected`, investigation still records the governed read, and the policy request fails with `connector_llm_not_linked` instead of inventing a recommendation.

## How a case runs

1. The customer submits the complaint. CasePilot inserts the case, then asks Connector to register an investigator, a policy analyst, and a resolution agent, and opens one workbench session for each.
2. The caseworker runs investigation. CasePilot asks Connector to read the order. Only if that admission proceeds does the order body enter the evidence. CasePilot then calls Connector Talk as the investigator, with the admitted evidence in the user message.
3. The caseworker asks for policy. Connector Talk answers as the policy agent. CasePilot stores that text as `recommendation_only`. The policy agent cannot approve the refund.
4. The caseworker proposes a replacement or a refund on order `ord_4401`. CasePilot calls Connector tool invoke as the resolution agent. A refund of 12900 cents is above the default 5000-cent limit, so the first success from Connector still does not create the refund until a supervisor approval exists and a second admission succeeds.
5. The supervisor approves. CasePilot records who approved, the scope, and the expiry, tells Connector if a human-approval id was returned, and submits the effect again.
6. CasePilot reads the replacement or refund row it just wrote and marks the outcome `verified` only when that row matches and Connector returned a task id or receipt. Otherwise the status stays unverified.
7. The supervisor can cease. Later effects on that generation are refused. Resume registers a new generation with Connector before any further effect.

## Setup

You need Connector OS already installed and listening, normally at `http://127.0.0.1:9091`. CasePilot can install a released tarball, not a git checkout:

```bash
# edit deploy/connector.pin and set url and sha256 from the release
./deploy/install-connector.sh
```

On Connector, create an API key and link the model. The link call is Connector's, and the key in it is the model vendor key:

```bash
curl -s -X POST "$CONNECTOR_API_URL/api/v1/settings/llms/link" \
  -H "Authorization: Bearer $CONNECTOR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"provider":"openai","model":"gpt-4o-mini","api_key":"'"$VENDOR_KEY"'","ping":true}'
```

A local OpenAI-compatible server is the same call with `provider` of `ollama`, `lmstudio`, `vllm`, or `custom`, plus `endpoint` when the provider requires a base URL. Confirm with:

```bash
curl -s -H "Authorization: Bearer $CONNECTOR_API_KEY" \
  "$CONNECTOR_API_URL/api/v1/settings/llms/status"
```

`effective_mode` should not be `disconnected`.

Then start CasePilot:

```bash
cd /home/umesh/Projects/casepilot
npm install
npm run build
CONNECTOR_API_URL=http://127.0.0.1:9091 \
CONNECTOR_API_KEY=cpk_... \
cargo run
```

Open `http://127.0.0.1:8088/`. The first page explains the desk, asks for a model, and then opens Report, Desk, and Review. The vendor key is posted to Connector and is not stored in CasePilot.

## Deploy on Render

The public repository is the source Render builds. `render.yaml` is one free web service. That container runs CasePilot, Connector, and Surreal together. The Connector API key in the container is `dev-token`. The local preset accepts that key, so CasePilot connects in the backend after deploy without anyone pasting it. The model vendor key is not in the file. A person enters an email and their own model on the front page. Each email is a separate trial.

The free plan has no disk. A trial lasts while the instance is awake. Sleep or a restart clears the cases and the linked model. This Connector process admits tools inside the container. It is not the linux-kvm profile.

When the public URL answers `GET /healthz`, open `/`. Enter an email, link a model, then try Report.

## License

Apache License 2.0. See `LICENSE`.

`CASEPILOT_MODEL` is optional. If Connector's status already names a model, that name is what Talk uses. Set `CASEPILOT_MODEL` only when you need to name the model id CasePilot should send and Connector does not yet report one. `CASEPILOT_REFUND_AUTO_LIMIT_CENTS` defaults to `5000`. `CASEPILOT_BIND` defaults to `127.0.0.1:8088`.

## Using the screens

Customer portal. Sign in as `amina.shah` with customer `cust_amina`. Submit the complaint. The case is stored even if Connector refuses registration. The status then stays `intake_unbound`, and no remedy can run until Connector accepts the three agents.

Workstation. Sign in as `operator.nolan`. Open the case. Use Investigate, then Ask policy, then Propose replacement or Propose refund. Agent lane shows the Connector gateway and the three intelligence ids for an external agent. Evidence shows digests and the verified flag.

Supervisor. Sign in as `supervisor.lee`. Refresh approvals and approve or deny. Put the case id in the box to cease or resume. Deployment truth runs Connector's health check and, when `connectorctl` is installed, `product status` and `govern deploy-verify linux-kvm`. A backend that is not `READY` is unproven. Cosign on Connector checks the release artifact. It is not proof that a refund happened.

## Pointing an external agent at Connector

If the reasoning already lives in another program, do not add a second tool loop inside CasePilot. Point that program at Connector:

```text
OPENAI_BASE_URL=http://127.0.0.1:9091/v1
OPENAI_API_KEY=<Connector API key>
```

Each request uses Connector's chat parameters: `model`, `messages`, and `agent_pid` set to the investigator, policy, or resolution id from Agent lane. The external program still does not call the refund service. The caseworker proposes the effect in CasePilot, and Connector admits it.

## Seeded records

| Record | Value |
| --- | --- |
| Customer | `cust_amina`, Amina Shah |
| Order | `ord_4401`, studio headphones, 12900 cents, `delivered_damaged` |
| Shipment | `shp_4401`, `delayed` |

The order service, shipment, and refund ledger are inside this process, on localhost. They change only after Connector admits the matching tool.

## What this build does not claim

Connector's current tool invoke admits inside the Connector process and returns an outcome. It does not POST CasePilot's tool URL. CasePilot therefore applies the business write only after that outcome is `Success` or `proceed`, then checks the row. A direct call to `/internal/tools/...` returns 409 and writes nothing.

If `CONNECTOR_API_KEY` is missing, Connector answers 401, the case stays unbound, and no refund is created.
