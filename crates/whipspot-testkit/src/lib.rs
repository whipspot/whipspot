//! Fakes for tests: an in-memory credential store, a canned provider and a fake audio output, plus
//! the seeded generator and fuzz loop of the parser fuzz tests (`rng`).
//! A dev-dependency of the crates that test against them; nothing in a product crate may depend
//! on it.

pub mod audio_files;
mod contract;
pub mod cover;
pub mod demo;
mod fake_art;
mod fake_folders;
mod fake_lyrics;
mod fake_output;
mod fake_power;
mod fake_provider;
mod fake_skin;
mod fake_windows;
mod memory_store;
pub mod rng;

pub use contract::{
    LyricsOutcome, run_art_error_contract, run_art_source_contract, run_audio_output_contract,
    run_credential_store_contract, run_fake_windows_contract, run_lyrics_outcome_contract,
    run_lyrics_source_contract, run_music_folders_contract, run_panel_windows_contract,
    run_power_control_contract, run_provider_contract, run_provider_playback_contract,
    run_provider_session_contract, run_remote_contract, run_skin_caps_contract,
    run_skin_engine_contract, run_skin_source_contract, run_skin_source_size_contract,
};
pub use demo::DemoProvider;
pub use fake_art::{FakeArt, Gate, GateWait, demo_images};
pub use fake_folders::FakeFolders;
pub use fake_lyrics::{FakeLyrics, demo_lyrics};
pub use fake_output::{ClockedOutput, FakeOutput};
pub use fake_power::FakePower;
pub use fake_provider::FakeProvider;
pub use fake_skin::{FakeSkinEngine, FakeSkinSource};
pub use fake_windows::FakeWindows;
pub use memory_store::MemoryStore;
