//! [`AuthFlow`] for Spotify: two steps, `playback` (no input) and `web` (input `clientId`), and
//! on a provider built with Connect a third, `connect` (no input): its button turns the Connect
//! speaker on when it is off and off when it is on, its logout turns it off. Unknown steps and
//! unknown or missing inputs fail closed with fixed texts.

use crate::{
    auth_manager::{AuthError, AuthStatus},
    provider::{INPUT_CLIENT_ID, STEP_CONNECT, STEP_PLAYBACK, STEP_WEB, SpotifyProvider},
};
use whipspot_core::provider::{
    AuthFailure, AuthFlow, AuthStepStatus, BoxFuture, LoginInput, ProviderAuthStatus,
};

const UNKNOWN_STEP: &str = "unknown login step";
const UNEXPECTED_INPUT: &str = "unexpected login input";
const INTERNAL: &str = "internal error";

fn fail(e: AuthError) -> AuthFailure {
    AuthFailure(e.to_string())
}

impl From<AuthStatus> for ProviderAuthStatus {
    fn from(s: AuthStatus) -> Self {
        ProviderAuthStatus {
            steps: vec![
                AuthStepStatus {
                    id: STEP_PLAYBACK.into(),
                    state: s.playback,
                    action: None,
                    detail: None,
                },
                AuthStepStatus {
                    id: STEP_WEB.into(),
                    state: s.web,
                    action: None,
                    detail: None,
                },
            ],
        }
    }
}

impl SpotifyProvider {
    /// The neutral status: the two login steps, and the Connect step when there is one.
    async fn provider_status(&self) -> Result<ProviderAuthStatus, AuthFailure> {
        let auth = self.read_status().await?;
        let playback = auth.playback.clone();
        let mut status = ProviderAuthStatus::from(auth);
        if let Some(c) = &self.connect {
            status.steps.push(c.step(&playback));
        }
        Ok(status)
    }

    /// The Spotify-shaped status behind `status()`. Reads the credential store, which may block on a
    /// system prompt: kept off the async workers.
    async fn read_status(&self) -> Result<AuthStatus, AuthFailure> {
        let auth = self.auth.clone();
        tokio::task::spawn_blocking(move || auth.status())
            .await
            .map_err(|_| AuthFailure(INTERNAL.to_owned()))
    }
}

impl AuthFlow for SpotifyProvider {
    fn status(&self) -> BoxFuture<'_, Result<ProviderAuthStatus, AuthFailure>> {
        Box::pin(self.provider_status())
    }

    fn login<'a>(
        &'a self,
        step: &'a str,
        input: &'a LoginInput,
    ) -> BoxFuture<'a, Result<ProviderAuthStatus, AuthFailure>> {
        Box::pin(async move {
            // Validate the step and its inputs before any backend or store work.
            match step {
                STEP_PLAYBACK => {
                    if !input.0.is_empty() {
                        return Err(AuthFailure(UNEXPECTED_INPUT.to_owned()));
                    }
                    let result = self.auth.login_playback().await;
                    // Only an attempt that changed the login state ends the session the player
                    // used (S1, S3): `Ok` stored a login that may be another account, and
                    // `Err(CredentialStore)` is a failed write that cleared the login. Every
                    // other error changed nothing (not completed, timed out, port in use,
                    // refused while another login runs): the session still runs on the login
                    // that is stored, so it stays (F9). `Slot::login_inner` names this rule.
                    // Connect follows the same rule (MD137): a new login restarts it on the new
                    // token, a failed write ends it.
                    if matches!(result, Ok(()) | Err(AuthError::CredentialStore)) {
                        self.host.end_session().await;
                    }
                    if let Some(c) = &self.connect {
                        match result {
                            Ok(()) => c.restart().await,
                            Err(AuthError::CredentialStore) => c.stop().await,
                            Err(_) => {}
                        }
                    }
                    result.map_err(fail)?;
                }
                STEP_WEB => {
                    if input.0.keys().any(|k| k != INPUT_CLIENT_ID) {
                        return Err(AuthFailure(UNEXPECTED_INPUT.to_owned()));
                    }
                    // A missing id is the same failure as a malformed one; `login_web`
                    // validates the shape before anything else happens.
                    let id = input.get(INPUT_CLIENT_ID).unwrap_or("");
                    self.auth.login_web(id).await.map_err(fail)?;
                    self.api.forget_user();
                    self.watch.reset();
                }
                STEP_CONNECT if self.connect.is_some() => {
                    if !input.0.is_empty() {
                        return Err(AuthFailure(UNEXPECTED_INPUT.to_owned()));
                    }
                    // The step's one button reads "Turn off" while on (MD177: the action
                    // button calls login in any state), so it turns Connect the other way.
                    if let Some(c) = &self.connect {
                        c.set_enabled(!c.enabled()).await;
                    }
                }
                _ => return Err(AuthFailure(UNKNOWN_STEP.to_owned())),
            }
            self.provider_status().await
        })
    }

    fn logout<'a>(
        &'a self,
        step: &'a str,
    ) -> BoxFuture<'a, Result<ProviderAuthStatus, AuthFailure>> {
        Box::pin(async move {
            // Every per-account resource ends with the login, also when deleting the stored
            // login failed (the in-memory token is gone either way): the playback session and
            // its player, or the cached web user id (S1).
            match step {
                STEP_PLAYBACK => {
                    let result = self.auth.logout_playback().await;
                    self.host.end_session().await;
                    if let Some(c) = &self.connect {
                        c.stop().await;
                    }
                    result.map_err(fail)?;
                }
                STEP_WEB => {
                    let result = self.auth.logout_web().await;
                    self.api.forget_user();
                    self.watch.reset();
                    result.map_err(fail)?;
                }
                STEP_CONNECT if self.connect.is_some() => {
                    if let Some(c) = &self.connect {
                        c.set_enabled(false).await;
                    }
                }
                _ => return Err(AuthFailure(UNKNOWN_STEP.to_owned())),
            }
            self.provider_status().await
        })
    }

    fn prefill<'a>(
        &'a self,
        step: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, AuthFailure>> {
        Box::pin(async move {
            if step != STEP_WEB || input != INPUT_CLIENT_ID {
                return Err(AuthFailure(UNKNOWN_STEP.to_owned()));
            }
            // Reads the credential store, which may block on a system prompt.
            let auth = self.auth.clone();
            tokio::task::spawn_blocking(move || auth.web_client_id())
                .await
                .map_err(|_| AuthFailure(INTERNAL.to_owned()))?
                .map_err(fail)
        })
    }
}
