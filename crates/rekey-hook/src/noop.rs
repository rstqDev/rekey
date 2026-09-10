//! Stand-in platform used on unsupported targets and in tests.
//!
//! It records what it was asked to do rather than touching a real keyboard,
//! which is what lets the end-to-end engine tests run in CI on Linux.

use crate::{Context, HookError, Injector, KeyEvent};
use std::sync::{Arc, Mutex};

#[derive(Debug, Default, Clone)]
pub struct RecordingInjector {
    pub actions: Arc<Mutex<Vec<Action>>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Backspace(usize),
    Type(String),
}

impl RecordingInjector {
    pub fn new() -> RecordingInjector {
        RecordingInjector::default()
    }

    pub fn taken(&self) -> Vec<Action> {
        std::mem::take(&mut self.actions.lock().unwrap())
    }
}

impl Injector for RecordingInjector {
    fn backspace(&self, count: usize) {
        self.actions.lock().unwrap().push(Action::Backspace(count));
    }

    fn type_text(&self, text: &str) {
        self.actions
            .lock()
            .unwrap()
            .push(Action::Type(text.to_string()));
    }
}

pub fn current_context() -> Context {
    Context::default()
}

pub fn has_permission() -> bool {
    false
}

/// No keyboard to switch on an unsupported platform.
pub fn select_layout(_layout_id: &str) -> bool {
    false
}

pub fn request_permission() {}

pub fn run<F>(_on_key: F) -> Result<(), HookError>
where
    F: FnMut(KeyEvent) + Send + 'static,
{
    Err(HookError::Unsupported(
        "keyboard hooks are available on macOS and Windows",
    ))
}
