//! The real power control on this machine (efficiency mode R1-3). It changes only this test
//! process, and every test ends at Normal. The tests share the process-wide mode, so they run one
//! at a time.

use std::sync::{Mutex, MutexGuard};

use whipspot_app::power::{OsPower, current_thread_saving, sync_current_thread};
use whipspot_core::power::{PowerControl, PowerMode};
use whipspot_testkit::run_power_control_contract;

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn supported() -> bool {
    cfg!(any(windows, target_os = "macos"))
}

#[test]
fn os_power_passes_the_power_control_contract() {
    let _serial = serial();
    run_power_control_contract(&OsPower::new());
}

#[test]
fn a_thread_that_syncs_is_marked_in_eco_and_restored_in_normal() {
    let _serial = serial();
    let power = OsPower::new();
    let (before, marked, after) = std::thread::scope(|s| {
        s.spawn(|| {
            sync_current_thread();
            let before = current_thread_saving();
            let _ = power.set(PowerMode::Eco);
            sync_current_thread();
            let marked = current_thread_saving();
            let _ = power.set(PowerMode::Normal);
            sync_current_thread();
            (before, marked, current_thread_saving())
        })
        .join()
        .unwrap()
    });
    let _ = power.set(PowerMode::Normal);
    if supported() {
        assert_eq!(before, Some(false), "a new thread starts unmarked");
        assert_eq!(marked, Some(true), "the thread marked itself in Eco");
        assert_eq!(after, before, "Normal gave the original state back");
    } else {
        assert_eq!((before, marked, after), (None, None, None));
    }
}

#[test]
fn a_thread_that_never_syncs_stays_unmarked() {
    let _serial = serial();
    let power = OsPower::new();
    let (before, during) = std::thread::scope(|s| {
        s.spawn(|| {
            let before = current_thread_saving();
            let _ = power.set(PowerMode::Eco);
            let during = current_thread_saving();
            let _ = power.set(PowerMode::EcoIdle);
            (before, current_thread_saving().or(during))
        })
        .join()
        .unwrap()
    });
    let _ = power.set(PowerMode::Normal);
    if supported() {
        assert_eq!(before, Some(false));
        assert_eq!(
            during,
            Some(false),
            "a thread that never syncs (every audio thread, I5) is never marked"
        );
    }
}

#[test]
fn eco_marks_and_normal_restores_the_main_flow_across_repeats() {
    // Eco -> EcoIdle -> Eco keeps a synced thread marked; Normal restores it once.
    let _serial = serial();
    let power = OsPower::new();
    let states = std::thread::scope(|s| {
        s.spawn(|| {
            let mut states = Vec::new();
            for mode in [
                PowerMode::Eco,
                PowerMode::EcoIdle,
                PowerMode::Eco,
                PowerMode::Normal,
                PowerMode::Normal,
            ] {
                let _ = power.set(mode);
                sync_current_thread();
                states.push(current_thread_saving());
            }
            states
        })
        .join()
        .unwrap()
    });
    let _ = power.set(PowerMode::Normal);
    if supported() {
        assert_eq!(
            states,
            [Some(true), Some(true), Some(true), Some(false), Some(false)]
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_eco_idle_sets_process_ecoqos_and_the_idle_class_and_normal_restores_them() {
    use whipspot_app::power::current_process_state;
    const EXECUTION_SPEED: u32 = 1;
    const IDLE_PRIORITY_CLASS: u32 = 0x40;
    let _serial = serial();
    let power = OsPower::new();
    let (_, _, original) = current_process_state().expect("process state readable");
    assert_ne!(original, IDLE_PRIORITY_CLASS);

    assert_eq!(power.set(PowerMode::EcoIdle), Ok(()));
    assert_eq!(
        current_process_state(),
        Some((EXECUTION_SPEED, EXECUTION_SPEED, IDLE_PRIORITY_CLASS))
    );

    // Leaving EcoIdle for Eco (the step before an output opens) restores the class (I1).
    assert_eq!(power.set(PowerMode::Eco), Ok(()));
    let (control, _, class) = current_process_state().unwrap();
    assert_eq!((control, class), (0, original));

    assert_eq!(power.set(PowerMode::EcoIdle), Ok(()));
    assert_eq!(power.set(PowerMode::Normal), Ok(()));
    let (control, _, class) = current_process_state().unwrap();
    assert_eq!((control, class), (0, original));
}
