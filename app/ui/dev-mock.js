// Development stand-in for the Tauri bridge.
//
// Loaded before app.js and inert whenever real Tauri is present. It lets the
// settings window be opened in an ordinary browser — for styling, for checking
// light and dark rendering, and for capturing screenshots — without rebuilding
// the Rust side. The responses mirror the shapes returned by the `#[tauri::command]`
// functions in src/main.rs; if you change one there, change it here too.

/**
 * True only for a plain dev server on localhost.
 *
 * Tauri serves the UI from `tauri://localhost` on macOS and
 * `http://tauri.localhost` on Windows, so neither matches. Checking merely
 * that `window.__TAURI__` is missing is not enough: if the bridge ever fails
 * to inject, the app would quietly show invented statistics instead of
 * failing, which is far worse than showing nothing.
 */
function isLocalDevServer() {
  const httpish = location.protocol === "http:" || location.protocol === "https:";
  const local = location.hostname === "localhost" || location.hostname === "127.0.0.1";
  return httpish && local;
}

if (!window.__TAURI__ && isLocalDevServer()) {
  const LAYOUTS = [
    ["us", "English (US QWERTY)", "en", false],
    ["ru", "Russian (ЙЦУКЕН)", "ru", false],
    ["uk", "Ukrainian (ЙЦУКЕН)", "uk", false],
    ["he", "Hebrew", "he", false],
    ["ar", "Arabic (101)", "ar", false],
    ["el", "Greek", "el", false],
    ["de", "German (QWERTZ)", "de", true],
    ["fr", "French (AZERTY)", "fr", true],
    ["es", "Spanish (QWERTY)", "es", true],
    ["tr", "Turkish (Q)", "tr", true],
  ];

  // Enough of the real conversion to make the preview box honest in the demo.
  const SAMPLES = {
    "ghbdtn": "привет",
    "rfr": "как",
    "ltkf": "дела",
    "cgfcb,j": "спасибо",
    "yjhv": "норм",
    "руддщ": "hello",
  };

  const state = {
    enabled: true,
    layouts: ["us", "ru"],
    sensitivity: "balanced",
    skip_all_caps: true,
    ai_assist: false,
    has_api_key: false,
    launch_at_login: true,
    excluded_apps: [
      "com.apple.keychainaccess",
      "com.1password.1password",
      "com.bitwarden.desktop",
      "com.apple.Terminal",
      "com.googlecode.iterm2",
    ],
    exceptions: [],
    shortcut: "tap:option",
    assist_active: false,
    assist_learned: 0,
    vibrancy: true,
    corrections: 1284,
    undos: 3,
    has_permission: true,
    hook_running: true, // flips on by itself once Accessibility is granted
    available_layouts: LAYOUTS.map(([id, name, language, limited]) => ({
      id, name, language, limited,
    })),
    models_loaded: ["ar", "de", "el", "en", "es", "fr", "he", "ru", "tr", "uk"],
    version: "0.1.0",
    // Follow the host so the dev server previews the right skin.
    platform: globalThis.navigator?.userAgent?.includes("Windows")
      ? "windows"
      : "macos",
  };

  const handlers = {
    get_state: () => structuredClone(state),
    set_config: ({ config, launchAtLogin }) => {
      Object.assign(state, config);
      if (launchAtLogin !== undefined) state.launch_at_login = launchAtLogin;
    },
    request_permission: () => true,
    open_accessibility_settings: () => {},
    undo: () => {},
    preview: ({ text }) => {
      let changed = false;
      const output = text
        .split(/\s+/)
        .map((word) => {
          const fixed = SAMPLES[word.toLowerCase()];
          if (fixed) changed = true;
          return fixed ?? word;
        })
        .join(" ");
      return { output, changed };
    },
  };

  window.__TAURI__ = {
    core: {
      invoke: async (name, args = {}) => {
        const handler = handlers[name];
        if (!handler) throw new Error(`no mock for command ${name}`);
        return handler(args);
      },
    },
  };
  console.info("Rekey UI running against the development mock.");
}
