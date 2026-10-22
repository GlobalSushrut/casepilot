-- The case file is this graph, not a JSON dossier beside the tables.
-- One walk goes from the customer, along what was said, through the agents
-- and the human decision, to the Connector receipt.
-- An effect row is rejected unless that same row carries verdict "proceed".
-- That is the split the tables cannot close: a refund existing without the
-- admission sitting on the same record.

DEFINE TABLE IF NOT EXISTS customer SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS customer_id ON customer TYPE string;
DEFINE FIELD IF NOT EXISTS name ON customer TYPE string;

DEFINE TABLE IF NOT EXISTS case_file SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS case_id ON case_file TYPE string;
DEFINE FIELD IF NOT EXISTS summary ON case_file TYPE string;
DEFINE FIELD IF NOT EXISTS status ON case_file TYPE string;
DEFINE FIELD IF NOT EXISTS at ON case_file TYPE string;
DEFINE INDEX IF NOT EXISTS case_file_id ON case_file FIELDS case_id UNIQUE;

DEFINE TABLE IF NOT EXISTS utterance SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS case_id ON utterance TYPE string;
DEFINE FIELD IF NOT EXISTS direction ON utterance TYPE string;
DEFINE FIELD IF NOT EXISTS body ON utterance TYPE string;
DEFINE FIELD IF NOT EXISTS at ON utterance TYPE string;

DEFINE TABLE IF NOT EXISTS charter SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS case_id ON charter TYPE string;
DEFINE FIELD IF NOT EXISTS role ON charter TYPE string;
DEFINE FIELD IF NOT EXISTS pid ON charter TYPE string;

DEFINE TABLE IF NOT EXISTS decision SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS case_id ON decision TYPE string;
DEFINE FIELD IF NOT EXISTS kind ON decision TYPE string;
DEFINE FIELD IF NOT EXISTS note ON decision TYPE string;
DEFINE FIELD IF NOT EXISTS by_whom ON decision TYPE string;

DEFINE TABLE IF NOT EXISTS receipt SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS case_id ON receipt TYPE string;
DEFINE FIELD IF NOT EXISTS action_id ON receipt TYPE string;
DEFINE FIELD IF NOT EXISTS effect_type ON receipt TYPE string;
DEFINE FIELD IF NOT EXISTS verdict ON receipt TYPE string;
DEFINE FIELD IF NOT EXISTS task_id ON receipt TYPE option<string>;

DEFINE TABLE IF NOT EXISTS effect SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS case_id ON effect TYPE string;
DEFINE FIELD IF NOT EXISTS action_id ON effect TYPE string;
DEFINE FIELD IF NOT EXISTS kind ON effect TYPE string;
DEFINE FIELD IF NOT EXISTS verdict ON effect TYPE string;
DEFINE FIELD IF NOT EXISTS receipt_id ON effect TYPE string;
DEFINE EVENT IF NOT EXISTS effect_requires_proceed ON effect WHEN $event = "CREATE" THEN {
  IF $after.verdict != "proceed" OR $after.receipt_id = NONE {
    THROW "An effect cannot be stored unless the receipt verdict is proceed.";
  };
};

DEFINE FIELD IF NOT EXISTS party_id ON case_file TYPE option<string>;
DEFINE FIELD IF NOT EXISTS subject_ref ON case_file TYPE option<string>;
DEFINE FIELD IF NOT EXISTS matter_key ON case_file TYPE option<string>;
DEFINE INDEX IF NOT EXISTS case_party ON case_file FIELDS party_id;

DEFINE TABLE IF NOT EXISTS matter SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS matter_key ON matter TYPE string;
DEFINE FIELD IF NOT EXISTS party_id ON matter TYPE string;
DEFINE FIELD IF NOT EXISTS subject_ref ON matter TYPE string;
DEFINE INDEX IF NOT EXISTS matter_once ON matter FIELDS matter_key UNIQUE;

DEFINE ANALYZER IF NOT EXISTS case_text TOKENIZERS class FILTERS lowercase;
DEFINE INDEX IF NOT EXISTS utterance_words ON utterance FIELDS body SEARCH ANALYZER case_text BM25;

DEFINE TABLE IF NOT EXISTS filed TYPE RELATION IN customer OUT case_file;
DEFINE TABLE IF NOT EXISTS has TYPE RELATION IN customer OUT matter;
DEFINE TABLE IF NOT EXISTS opened TYPE RELATION IN matter OUT case_file;
DEFINE TABLE IF NOT EXISTS said TYPE RELATION IN case_file OUT utterance;
DEFINE TABLE IF NOT EXISTS bound TYPE RELATION IN case_file OUT charter;
DEFINE TABLE IF NOT EXISTS decided TYPE RELATION IN case_file OUT decision;
DEFINE TABLE IF NOT EXISTS admitted TYPE RELATION IN case_file OUT receipt;
DEFINE TABLE IF NOT EXISTS cites TYPE RELATION IN decision OUT receipt;
DEFINE TABLE IF NOT EXISTS follows TYPE RELATION IN effect OUT receipt;
