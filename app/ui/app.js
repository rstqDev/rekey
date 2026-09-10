// Rekey settings window.
//
// The window is a thin view over the engine: it reads a single snapshot from
// Rust, and every change sends the whole config back. Keeping one source of
// truth on the Rust side avoids the UI and the engine drifting apart.

const invoke = window.__TAURI__.core.invoke;

const SENSITIVITY_HINTS = {
  cautious: "Only acts on overwhelming evidence. Rarely wrong, misses more.",
  balanced: "Acts when the other reading is clearly a real word. Recommended.",
  eager: "Acts on weaker evidence. Catches more slang, occasionally overreaches.",
};

/** Last snapshot from Rust; the basis for every save. */
let state = null;
/** True while applying a snapshot, so programmatic changes don't re-save. */
let loading = false;

const $ = (id) => document.getElementById(id);

async function refresh() {
  state = await invoke("get_state");
  render();
}

function render() {
  loading = true;

  $("enabled").checked = state.enabled;
  $("permission").hidden = state.has_permission && state.hook_running;
  $("no-models").hidden = state.models_loaded.length > 0;
  $("version").textContent = `v${state.version}`;

  renderStats();
  renderLayouts();
  renderSensitivity();

  $("skip-caps").checked = state.skip_all_caps;
  $("launch").checked = state.launch_at_login;
  $("ai").checked = state.ai_assist;
  $("ai-key-row").hidden = !state.ai_assist;
  // Never echo a stored key back into the DOM; show that one exists instead.
  $("api-key").placeholder = state.has_api_key ? "key saved — type to replace" : "sk-ant-…";

  renderChips("excluded", state.excluded_apps);
  $("exceptions-section").hidden = state.exceptions.length === 0;
  renderChips("exceptions", state.exceptions);

  loading = false;
  updatePreview();
}

function renderStats() {
  const { corrections, undos } = state;
  if (corrections === 0) {
    $("stats").textContent = "No corrections yet";
    return;
  }
  const word = corrections === 1 ? "correction" : "corrections";
  $("stats").textContent =
    undos > 0
      ? `${corrections} ${word} · ${undos} undone`
      : `${corrections} ${word}`;
}

function renderLayouts() {
  const list = $("layouts");
  list.textContent = "";
  for (const layout of state.available_layouts) {
    const li = document.createElement("li");
    const label = document.createElement("label");

    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = state.layouts.includes(layout.id);
    box.addEventListener("change", () => {
      const next = new Set(state.layouts);
      box.checked ? next.add(layout.id) : next.delete(layout.id);
      save({ layouts: [...next] });
    });

    const name = document.createElement("span");
    name.className = "name";
    name.textContent = layout.name;

    label.append(box, name);
    if (layout.limited) {
      const badge = document.createElement("span");
      badge.className = "badge";
      badge.textContent = "limited";
      badge.title =
        "This layout shares the Latin alphabet with US QWERTY, so there is " +
        "little to tell the two apart. Rekey catches only the clearest cases.";
      label.append(badge);
    }
    li.append(label);
    list.append(li);
  }

  // The preview needs a layout to type "on"; offer the enabled ones.
  const select = $("try-layout");
  const previous = select.value;
  select.textContent = "";
  for (const id of state.layouts) {
    const info = state.available_layouts.find((l) => l.id === id);
    const option = document.createElement("option");
    option.value = id;
    option.textContent = info ? info.name : id;
    select.append(option);
  }
  select.value = state.layouts.includes(previous) ? previous : state.layouts[0] ?? "";
}

function renderSensitivity() {
  for (const button of $("sensitivity").querySelectorAll("button")) {
    const selected = button.dataset.value === state.sensitivity;
    button.setAttribute("aria-checked", String(selected));
  }
  $("sensitivity-hint").textContent = SENSITIVITY_HINTS[state.sensitivity] ?? "";
}

function renderChips(id, values) {
  const list = $(id);
  list.textContent = "";
  for (const value of values) {
    const li = document.createElement("li");
    li.textContent = value;
    list.append(li);
  }
}

/** Send the current config back to Rust with `changes` applied. */
async function save(changes = {}, extras = {}) {
  if (loading || !state) return;
  const config = {
    enabled: state.enabled,
    layouts: state.layouts,
    sensitivity: state.sensitivity,
    min_word_len: 2,
    require_dict_below_len: 4,
    exceptions: state.exceptions,
    excluded_apps: state.excluded_apps,
    skip_all_caps: state.skip_all_caps,
    ai_assist: state.ai_assist,
    ...changes,
  };
  await invoke("set_config", { config, ...extras });
  await refresh();
}

let previewTimer = null;

function updatePreview() {
  clearTimeout(previewTimer);
  previewTimer = setTimeout(async () => {
    const text = $("try-input").value || $("try-input").placeholder;
    const layout = $("try-layout").value;
    if (!layout) return;
    const result = await invoke("preview", { text, layout });
    const output = $("try-output");
    output.textContent = result.output;
    output.classList.toggle("unchanged", !result.changed);
  }, 90);
}

// -- wiring -----------------------------------------------------------------

$("enabled").addEventListener("change", (e) => save({ enabled: e.target.checked }));
$("skip-caps").addEventListener("change", (e) => save({ skip_all_caps: e.target.checked }));
$("ai").addEventListener("change", (e) => save({ ai_assist: e.target.checked }));
$("launch").addEventListener("change", (e) =>
  save({}, { launchAtLogin: e.target.checked })
);

$("api-key").addEventListener("change", (e) => {
  const key = e.target.value.trim();
  if (!key) return;
  e.target.value = "";
  save({}, { apiKey: key });
});

$("sensitivity").addEventListener("click", (e) => {
  const button = e.target.closest("button");
  if (button) save({ sensitivity: button.dataset.value });
});

$("try-input").addEventListener("input", updatePreview);
$("try-layout").addEventListener("change", updatePreview);

$("grant").addEventListener("click", async () => {
  await invoke("request_permission");
  setTimeout(refresh, 600);
});

$("open-settings").addEventListener("click", () =>
  invoke("open_accessibility_settings")
);

// Stats change while the user is typing elsewhere, so keep the footer live
// whenever the window is actually on screen.
setInterval(() => {
  if (!document.hidden) refresh();
}, 2500);

refresh();
