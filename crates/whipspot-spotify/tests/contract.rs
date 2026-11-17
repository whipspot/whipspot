//! Provider contract on a logged-out Spotify provider. Read-only cases only (MD7): no login
//! (it opens a browser), no playback (it opens a device), no socket.

use std::sync::Arc;

use tokio::sync::broadcast;
use whipspot_core::{
    engine::{Engine, EngineConfig},
    provider::ProviderContext,
};
use whipspot_spotify::SpotifyProvider;
use whipspot_testkit::{MemoryStore, run_provider_contract, run_provider_playback_contract};

fn logged_out_spotify() -> Arc<SpotifyProvider> {
    let (events, _rx) = broadcast::channel(16);
    let source = Engine::new(EngineConfig::default(), None).source;
    let ctx = ProviderContext { source, events };
    Arc::new(SpotifyProvider::new(Arc::new(MemoryStore::default()), ctx))
}

#[tokio::test]
async fn provider_contract_on_logged_out_spotify() {
    run_provider_contract(logged_out_spotify()).await;
}

/// Only the playback cases that need no session: a malformed uri is refused with a fixed text
/// before any login or device (a good uri would need a login, so none is given).
#[tokio::test]
async fn playback_contract_on_logged_out_spotify() {
    run_provider_playback_contract(logged_out_spotify(), None, "spotify:track:short").await;
}
