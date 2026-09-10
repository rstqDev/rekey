// Rekey settings window.
//
// The window is a thin view over the engine: it reads a single snapshot from
// Rust, and every change sends the whole config back. Keeping one source of
// truth on the Rust side avoids the UI and the engine drifting apart.

// Provided by Tauri (`withGlobalTauri`), or by dev-mock.js on a dev server.
const bridge = window.__TAURI__;
const invoke = bridge?.core?.invoke;

/** Modifier names differ between platforms; the keys are the same. */
const MODIFIER_NAMES = {
  macos: { option: "Option", shift: "Shift", control: "Control", command: "Command" },
  windows: { option: "Alt", shift: "Shift", control: "Ctrl", command: "Windows key" },
};

/** Chords that already mean something, so a single tap is a poorer choice. */
const BUSY_MODIFIER = {
  macos: {
    option: "Option+Backspace and Option+Arrow are ordinary macOS shortcuts — " +
      "those still work, and never trigger this.",
  },
  windows: {
    option: "Alt+Tab and Alt+F4 are ordinary Windows shortcuts — those still " +
      "work, and never trigger this.",
  },
};

/** Describe the selected trigger in the platform's own terms. */
function shortcutHint(value, platform) {
  if (!value || value === "off") {
    return "No manual shortcut. Rekey only corrects on its own.";
  }
  const [kind, modifier] = value.split(":");
  const names = MODIFIER_NAMES[platform] ?? MODIFIER_NAMES.macos;
  const name = names[modifier] ?? modifier;
  const busy = BUSY_MODIFIER[platform]?.[modifier];

  if (kind === "doubletap") {
    return `Two quick taps of ${name}.` +
      (busy ? " Safer than a single tap if you use it in chords." : "");
  }
  return `One tap of ${name}, alone.` + (busy ? ` ${busy}` : "");
}

/** Relabel the trigger menu for the platform. */
function renderShortcutOptions(platform) {
  const names = MODIFIER_NAMES[platform] ?? MODIFIER_NAMES.macos;
  for (const option of $("shortcut").options) {
    if (option.value === "off") continue;
    const [kind, modifier] = option.value.split(":");
    const name = names[modifier] ?? modifier;
    option.textContent = kind === "doubletap" ? `Double-tap ${name}` : `Tap ${name}`;
  }
}

const SENSITIVITY_HINTS = {
  cautious: "Only acts on overwhelming evidence. Rarely wrong, misses more.",
  balanced: "Acts when the other reading is clearly a real word. Recommended.",
  eager: "Acts on weaker evidence. Catches more slang, occasionally overreaches.",
};

/** Last snapshot from Rust; the basis for every save. */
let state = null;

const $ = (id) => document.getElementById(id);

/** Read a fresh snapshot and rebuild the whole window. */
async function refresh() {
  state = await invoke("get_state");
  render();
}

/**
 * Refresh only the things that change on their own while the user is typing
 * elsewhere.
 *
 * The window must never rebuild its controls on a timer: doing so clobbers a
 * half-finished edit, and a click that lands during a rebuild is lost.
 */
async function pollLiveFields() {
  const fresh = await invoke("get_state");
  state.corrections = fresh.corrections;
  state.undos = fresh.undos;
  state.has_permission = fresh.has_permission;
  state.hook_running = fresh.hook_running;
  renderStats();
  $("permission").hidden = fresh.has_permission && fresh.hook_running;
}

function render() {
  // Style the window for the machine it is running on, not for a guess.
  document.documentElement.dataset.platform = state.platform;
  // Only let the background show through once the system has confirmed a
  // material is actually behind the window.
  document.documentElement.dataset.vibrancy = state.vibrancy ? "on" : "off";

  $("enabled").checked = state.enabled;
  $("enabled-detail").textContent = state.enabled
    ? "Watching for words typed on the wrong layout"
    : "Paused — nothing is being corrected";
  $("permission").hidden = state.has_permission && state.hook_running;
  $("no-models").hidden = state.models_loaded.length > 0;
  $("version").textContent = `v${state.version}`;

  renderStats();
  renderLayouts();
  renderSensitivity();

  renderShortcutOptions(state.platform);
  $("shortcut").value = state.shortcut;
  $("shortcut-hint").textContent = shortcutHint(state.shortcut, state.platform);

  // Only macOS gates keyboard access behind a permission, and only macOS has
  // a System Settings pane to send people to.
  $("open-settings").hidden = state.platform !== "macos";

  $("skip-caps").checked = state.skip_all_caps;
  $("launch").checked = state.launch_at_login;
  $("ai").checked = state.ai_assist;
  $("ai-key-row").hidden = !state.ai_assist;
  // Never echo a stored key back into the DOM; show that one exists instead.
  $("api-key").placeholder = state.has_api_key ? "key saved — type to replace" : "sk-ant-…";
  // Say plainly when the assist is on but cannot work, rather than looking
  // enabled and silently doing nothing.
  $("ai-status").textContent = !state.ai_assist
    ? ""
    : !state.assist_active
      ? "Needs an API key before it can do anything."
      : state.assist_learned > 0
        ? `${state.assist_learned} ambiguous ${
            state.assist_learned === 1 ? "word" : "words"
          } settled so far.`
        : "Waiting for a word the local models can't settle.";

  $("excluded-summary").title = state.excluded_apps.join("\n");
  $("exceptions-section").hidden = state.exceptions.length === 0;
  renderChips("exceptions", state.exceptions);

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
  if (!state) return;
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
    shortcut: parseShortcut(state.shortcut),
    ...changes,
  };
  await invoke("set_config", { config, ...extras });
  await refresh();
}

/** Turn "doubletap:option" into the tagged shape the engine expects. */
function parseShortcut(value) {
  if (!value || value === "off") return { kind: "off" };
  const [kind, modifier] = value.split(":");
  return { kind, modifier };
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

$("shortcut").addEventListener("change", (e) =>
  save({ shortcut: parseShortcut(e.target.value) })
);

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

// The correction count climbs while the user types in other apps, so keep the
// footer live — without touching any control they might be using.
setInterval(() => {
  if (!document.hidden && state) pollLiveFields();
}, 2500);

if (!invoke) {
  // Never fall back to plausible-looking placeholder data: an app that reads
  // your keyboard has to be honest about not working.
  document.body.innerHTML =
    '<div class="banner banner-error" style="margin:20px">' +
    "<p><strong>Rekey could not reach its backend.</strong> " +
    "The settings window cannot show or change anything. " +
    "Please reinstall Rekey and report this.</p></div>";
} else {
  refresh();
}
