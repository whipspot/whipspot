//! The start-up tool update (step 8): when it runs and when it must not. A fake `Http` stands in
//! for GitHub: `gets_seen` says whether anything was fetched.

mod fixtures;

use whipspot_core::provider::{LoginInput, Provider};
use whipspot_ytmusic::StartupUpdate;
use whipspot_ytmusic::auth::STEP_HELPER;
use whipspot_ytmusic::tools::STAMP_FILE;

use fixtures::Rig;

fn tools_dir(rig: &Rig) -> std::path::PathBuf {
    rig.dir.path().join("tools")
}

#[tokio::test]
async fn without_a_managed_copy_nothing_is_fetched() {
    let rig = Rig::new();
    assert_eq!(rig.provider.startup_update(), StartupUpdate::NoManagedCopy);
    assert!(rig.yt.gets_seen().is_empty(), "{:?}", rig.yt.gets_seen());
    assert!(!tools_dir(&rig).join(STAMP_FILE).exists());
}

#[tokio::test]
async fn with_the_helper_off_nothing_is_fetched() {
    let rig = Rig::new();
    rig.provider
        .auth()
        .login(STEP_HELPER, &LoginInput::default())
        .await
        .unwrap();
    // A due update, a managed copy, and the helper switched off by its log-out (D10).
    std::fs::remove_file(tools_dir(&rig).join(STAMP_FILE)).unwrap();
    rig.provider.auth().logout(STEP_HELPER).await.unwrap();
    let seen = rig.yt.gets_seen().len();
    assert_eq!(rig.provider.startup_update(), StartupUpdate::HelperOff);
    assert_eq!(rig.yt.gets_seen().len(), seen, "nothing was fetched");
}

#[tokio::test]
async fn a_managed_copy_is_checked_once_a_day() {
    let rig = Rig::new();
    rig.provider
        .auth()
        .login(STEP_HELPER, &LoginInput::default())
        .await
        .unwrap();
    // The install just made wrote the day's stamp.
    let seen = rig.yt.gets_seen().len();
    assert_eq!(rig.provider.startup_update(), StartupUpdate::NotDue);
    assert_eq!(rig.yt.gets_seen().len(), seen);
    // A day later (no stamp): the signed sums and signature are fetched, the tools are current.
    std::fs::remove_file(tools_dir(&rig).join(STAMP_FILE)).unwrap();
    assert_eq!(rig.provider.startup_update(), StartupUpdate::Checked);
    assert!(rig.yt.gets_seen().len() > seen, "the check fetched");
    assert_eq!(rig.provider.startup_update(), StartupUpdate::NotDue);
}
