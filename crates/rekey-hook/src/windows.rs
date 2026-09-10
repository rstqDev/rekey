//! Windows keyboard observation via a low-level hook, and injection via
//! `SendInput`.
//!
//! The shape mirrors the macOS implementation: `WH_KEYBOARD_LL` observes
//! keystrokes and always passes them on, and corrections are applied afterwards
//! as synthetic input. Windows needs no special permission for this, but the
//! hook does require a message pump on its own thread, and a process running
//! elevated will not see input from other elevated windows.

use crate::{Context, HookError, Injector, Key, KeyEvent, Modifiers};
use std::cell::RefCell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardLayout, GetKeyboardState, MapVirtualKeyW, SendInput, ToUnicodeEx,
    INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MAPVK_VK_TO_VSC, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME,
    VK_LEFT, VK_LWIN, VK_MENU, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_RWIN, VK_SHIFT, VK_SPACE,
    VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, MSG,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

/// Marker stamped into the `dwExtraInfo` of every synthetic event Rekey sends,
/// so the hook can recognise its own typing. `LLKHF_INJECTED` alone is not
/// enough: other well-behaved tools inject too, and their input is real input
/// as far as Rekey is concerned.
const REKEY_SIGNATURE: usize = 0x5245_4B45;

/// The hook callback receives no user pointer, so the handler is reached
/// through thread-local storage instead.
type Handler = Box<dyn FnMut(KeyEvent)>;

thread_local! {
    /// The hook callback is a bare `extern "system" fn` with no user pointer,
    /// so the handler has to be reachable from thread-local storage. The hook
    /// and its callback always live on the same thread.
    static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
}

// -- keyboard layout --------------------------------------------------------

/// Windows keyboard layout identifiers (the low word of an HKL) mapped to
/// Rekey layout ids.
const LAYOUT_IDS: &[(u16, &str)] = &[
    (0x0409, "us"), // English (United States)
    (0x0809, "us"), // English (United Kingdom)
    (0x0419, "ru"), // Russian
    (0x0422, "uk"), // Ukrainian
    (0x040D, "he"), // Hebrew
    (0x0401, "ar"), // Arabic (Saudi Arabia)
    (0x0C01, "ar"), // Arabic (Egypt)
    (0x0408, "el"), // Greek
    (0x0407, "de"), // German
    (0x0C07, "de"), // German (Austria)
    (0x040C, "fr"), // French
    (0x080C, "fr"), // French (Belgium)
    (0x0C0C, "fr"), // French (Canada)
    (0x0C0A, "es"), // Spanish (Spain)
    (0x080A, "es"), // Spanish (Mexico)
    (0x041F, "tr"), // Turkish
];

/// Map the language identifier in an HKL to a Rekey layout id.
pub fn layout_from_langid(langid: u16) -> Option<&'static str> {
    LAYOUT_IDS
        .iter()
        .find(|(id, _)| *id == langid)
        .map(|(_, layout)| *layout)
}

/// The layout of the thread that owns the foreground window — which is the one
/// the user is actually typing into, not necessarily our own.
pub fn current_layout() -> Option<String> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let thread = GetWindowThreadProcessId(hwnd, None);
        let hkl = GetKeyboardLayout(thread);
        let langid = (hkl.0 as usize & 0xFFFF) as u16;
        layout_from_langid(langid).map(|s| s.to_string())
    }
}

/// Executable name of the foreground application, e.g. `chrome.exe`.
pub fn frontmost_app() -> Option<String> {
    use windows::Win32::System::ProcessStatus::GetModuleBaseNameW;
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let handle = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
            false,
            pid,
        )
        .ok()?;
        let mut buf = [0u16; 260];
        let len = GetModuleBaseNameW(handle, None, &mut buf);
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        if len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Windows has no global secure-input flag; password fields are handled by the
/// app-exclusion list and by the engine's refusal to touch anything containing
/// digits or symbols.
pub fn secure_input_active() -> bool {
    false
}

pub fn current_context() -> Context {
    Context {
        app: frontmost_app(),
        layout: current_layout(),
        secure_input: secure_input_active(),
    }
}

/// Windows grants low-level keyboard hooks without a permission prompt.
pub fn has_permission() -> bool {
    true
}

pub fn request_permission() -> bool {
    true
}

// -- injection --------------------------------------------------------------

#[derive(Default)]
pub struct WindowsInjector;

impl WindowsInjector {
    pub fn new() -> Result<WindowsInjector, HookError> {
        Ok(WindowsInjector)
    }

    fn send(inputs: &[INPUT]) {
        unsafe {
            SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
        }
    }

    fn key_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
        let scan = unsafe { MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) } as u16;
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: scan,
                    dwFlags: if up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    time: 0,
                    dwExtraInfo: REKEY_SIGNATURE,
                },
            },
        }
    }

    /// One UTF-16 code unit as a synthetic keypress. Characters outside the
    /// basic plane are sent as their two surrogates, which Windows reassembles.
    fn unicode_input(unit: u16, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: windows::Win32::UI::Input::KeyboardAndMouse::INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: unit,
                    dwFlags: if up {
                        KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
                    } else {
                        KEYEVENTF_UNICODE
                    },
                    time: 0,
                    dwExtraInfo: REKEY_SIGNATURE,
                },
            },
        }
    }
}

impl Injector for WindowsInjector {
    fn backspace(&self, count: usize) {
        let mut inputs = Vec::with_capacity(count * 2);
        for _ in 0..count {
            inputs.push(WindowsInjector::key_input(VK_BACK, false));
            inputs.push(WindowsInjector::key_input(VK_BACK, true));
        }
        WindowsInjector::send(&inputs);
    }

    fn type_text(&self, text: &str) {
        // KEYEVENTF_UNICODE bypasses the active layout entirely, so the
        // corrected text arrives verbatim no matter which layout is selected.
        let mut inputs = Vec::new();
        for unit in text.encode_utf16() {
            inputs.push(WindowsInjector::unicode_input(unit, false));
            inputs.push(WindowsInjector::unicode_input(unit, true));
        }
        if !inputs.is_empty() {
            WindowsInjector::send(&inputs);
        }
    }
}

// -- the hook ---------------------------------------------------------------

fn modifiers_now() -> Modifiers {
    let down = |vk: VIRTUAL_KEY| unsafe { (GetKeyState(vk.0 as i32) as u16 & 0x8000) != 0 };
    Modifiers {
        shift: down(VK_SHIFT),
        control: down(VK_CONTROL),
        alt: down(VK_MENU),
        meta: down(VK_LWIN) || down(VK_RWIN),
    }
}

fn classify(vk: VIRTUAL_KEY, character: Option<char>) -> Key {
    match vk {
        VK_BACK => Key::Backspace,
        VK_SPACE => Key::Space,
        VK_RETURN => Key::Enter,
        VK_TAB => Key::Tab,
        VK_ESCAPE => Key::Escape,
        VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN | VK_HOME | VK_END | VK_PRIOR | VK_NEXT => {
            Key::Navigation
        }
        _ => {
            if character.is_some_and(|c| !c.is_control()) {
                Key::Character
            } else {
                Key::Other
            }
        }
    }
}

/// Resolve the character a virtual key produces under the foreground window's
/// layout and the current modifier state.
fn character_for(vk: u32, scan: u32) -> Option<char> {
    unsafe {
        let hwnd: HWND = GetForegroundWindow();
        let thread = if hwnd.0.is_null() {
            0
        } else {
            GetWindowThreadProcessId(hwnd, None)
        };
        let hkl = GetKeyboardLayout(thread);

        let mut state = [0u8; 256];
        if GetKeyboardState(&mut state).is_err() {
            return None;
        }
        let mut buf = [0u16; 8];
        // The final argument keeps ToUnicodeEx from disturbing the kernel's
        // dead-key state, which would corrupt the user's next keystroke.
        let n = ToUnicodeEx(vk, scan, &state, &mut buf, 1 << 2, Some(hkl));
        if n <= 0 {
            return None;
        }
        String::from_utf16_lossy(&buf[..n as usize]).chars().next()
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && (wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN) {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        // Only our own signature counts. `LLKHF_INJECTED` is deliberately not
        // consulted: other tools inject too, and their output is real input as
        // far as Rekey is concerned.
        let synthetic = info.dwExtraInfo == REKEY_SIGNATURE;
        let vk = VIRTUAL_KEY(info.vkCode as u16);
        let character = character_for(info.vkCode, info.scanCode);
        let event = KeyEvent {
            character,
            key: classify(vk, character),
            modifiers: modifiers_now(),
            synthetic,
        };
        HANDLER.with(|h| {
            if let Some(handler) = h.borrow_mut().as_mut() {
                handler(event);
            }
        });
    }
    // Always pass the keystroke on: Rekey observes, it never intercepts.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Install the hook and pump messages on the calling thread.
///
/// A low-level keyboard hook only delivers events to a thread with a message
/// loop, so this blocks and callers put it on a dedicated thread.
pub fn run<F>(on_key: F) -> Result<(), HookError>
where
    F: FnMut(KeyEvent) + Send + 'static,
{
    HANDLER.with(|h| *h.borrow_mut() = Some(Box::new(on_key)));

    let hook: HHOOK = unsafe {
        let module = GetModuleHandleW(None).map_err(|e| HookError::Os(e.to_string()))?;
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), Some(module.into()), 0)
            .map_err(|_| HookError::PermissionDenied)?
    };

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = UnhookWindowsHookEx(hook);
    }
    HANDLER.with(|h| *h.borrow_mut() = None);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_windows_langids_to_layouts() {
        assert_eq!(layout_from_langid(0x0409), Some("us"));
        assert_eq!(layout_from_langid(0x0419), Some("ru"));
        assert_eq!(layout_from_langid(0x0422), Some("uk"));
        assert_eq!(layout_from_langid(0x040D), Some("he"));
        assert_eq!(layout_from_langid(0x0408), Some("el"));
    }

    #[test]
    fn unknown_langids_are_not_guessed() {
        assert_eq!(layout_from_langid(0x0411), None); // Japanese
        assert_eq!(layout_from_langid(0x0404), None); // Chinese (Traditional)
    }

    #[test]
    fn classifies_editing_keys() {
        assert_eq!(classify(VK_BACK, None), Key::Backspace);
        assert_eq!(classify(VK_SPACE, Some(' ')), Key::Space);
        assert_eq!(classify(VK_LEFT, None), Key::Navigation);
        assert_eq!(classify(VIRTUAL_KEY(0x41), Some('a')), Key::Character);
    }
}
