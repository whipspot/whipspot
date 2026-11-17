//! The engine's one output adapter (MD43): shared mode through cpal on both OSes, exclusive
//! mode through WASAPI on Windows (`Unsupported` elsewhere).

use std::thread;

use whipspot_core::engine::{
    AudioOutput, OpenFailed, OutputDevice, OutputError, OutputInfo, OutputMode, OutputRequest,
    Renderer,
};

use crate::CpalOutput;
#[cfg(windows)]
use crate::wasapi_out::WasapiOutput;

/// Routes each open to the adapter for its mode. Opens nothing until `open`.
pub struct OutputRouter {
    shared: CpalOutput,
    #[cfg(windows)]
    exclusive: WasapiOutput,
    /// The mode of the inner adapter that is open.
    active: Option<OutputMode>,
}

impl OutputRouter {
    pub fn new() -> Self {
        Self {
            shared: CpalOutput::new(),
            #[cfg(windows)]
            exclusive: WasapiOutput::new(),
            active: None,
        }
    }
}

impl Default for OutputRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioOutput for OutputRouter {
    /// cpal's output devices (id = name = the friendly name, V12), listed on a short-lived
    /// thread so COM never initialises on the caller's thread.
    fn devices(&mut self) -> Result<Vec<OutputDevice>, OutputError> {
        thread::Builder::new()
            .name("whipspot-out-devices".into())
            .spawn(|| CpalOutput::new().devices())
            .map_err(|_| OutputError::Failed)?
            .join()
            .map_err(|_| OutputError::Failed)?
    }

    #[allow(clippy::result_large_err)]
    fn open(
        &mut self,
        request: &OutputRequest,
        renderer: Renderer,
    ) -> Result<OutputInfo, OpenFailed> {
        if self.active.is_some() {
            return Err(OpenFailed {
                error: OutputError::Busy,
                renderer,
            });
        }
        let result = match request.mode {
            OutputMode::Shared => self.shared.open(request, renderer),
            #[cfg(windows)]
            OutputMode::Exclusive => self.exclusive.open(request, renderer),
            #[cfg(not(windows))]
            OutputMode::Exclusive => Err(OpenFailed {
                error: OutputError::Unsupported,
                renderer,
            }),
        };
        if result.is_ok() {
            self.active = Some(request.mode);
        }
        result
    }

    fn close(&mut self) -> Option<Renderer> {
        match self.active.take()? {
            OutputMode::Shared => self.shared.close(),
            #[cfg(windows)]
            OutputMode::Exclusive => self.exclusive.close(),
            #[cfg(not(windows))]
            OutputMode::Exclusive => None,
        }
    }

    fn modes(&self) -> Vec<OutputMode> {
        if cfg!(windows) {
            vec![OutputMode::Shared, OutputMode::Exclusive]
        } else {
            vec![OutputMode::Shared]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_router_is_send() {
        fn is_send<T: Send>() {}
        is_send::<OutputRouter>();
    }

    #[test]
    fn new_opens_nothing() {
        let router = OutputRouter::new();
        assert!(router.active.is_none());
        #[cfg(windows)]
        assert!(!router.exclusive.is_open());
    }

    #[test]
    fn modes_per_os() {
        let modes = OutputRouter::new().modes();
        if cfg!(windows) {
            assert_eq!(modes, vec![OutputMode::Shared, OutputMode::Exclusive]);
        } else {
            assert_eq!(modes, vec![OutputMode::Shared]);
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn exclusive_is_unsupported_and_returns_the_renderer() {
        use whipspot_core::engine::{Engine, EngineConfig};

        let mut engine =
            Engine::new(EngineConfig::default(), Some(Box::new(OutputRouter::new()))).engine;
        let request = OutputRequest {
            mode: OutputMode::Exclusive,
            ..OutputRequest::default()
        };
        assert_eq!(engine.open(&request), Err(OutputError::Unsupported));
        // The renderer came back: a second attempt fails the same way, not for want of it.
        assert_eq!(engine.open(&request), Err(OutputError::Unsupported));
    }
}
