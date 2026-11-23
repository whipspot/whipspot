//! An in-memory [`PowerControl`]: it records every `set` and can be told to refuse. It changes no
//! process state.

use std::sync::Mutex;

use whipspot_core::power::{PowerControl, PowerError, PowerMode};

#[derive(Default)]
struct State {
    calls: Vec<PowerMode>,
    fail: Option<PowerError>,
}

/// A fake power control, named by the caller.
pub struct FakePower {
    name: &'static str,
    state: Mutex<State>,
}

impl FakePower {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            state: Mutex::new(State::default()),
        }
    }

    /// Every `set` so far, in order.
    pub fn calls(&self) -> Vec<PowerMode> {
        self.state.lock().unwrap().calls.clone()
    }

    /// The next and later `set`s return this error (`None` returns Ok again). Each call is
    /// recorded either way.
    pub fn fail_with(&self, error: Option<PowerError>) {
        self.state.lock().unwrap().fail = error;
    }
}

impl PowerControl for FakePower {
    fn name(&self) -> &'static str {
        self.name
    }

    fn set(&self, mode: PowerMode) -> Result<(), PowerError> {
        let mut state = self.state.lock().unwrap();
        state.calls.push(mode);
        match state.fail {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
