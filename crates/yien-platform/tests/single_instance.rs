//! One YIEN per data dir, claimed for real: the mutex or lock file and, on
//! Windows, the pipe the show request travels through.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use yien_platform::single_instance::{Claim, claim};

fn data_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("yien-instance-{}-{name}", std::process::id()))
}

#[test]
fn rf_ses_14_a_second_start_on_a_data_dir_asks_the_first_for_its_window() {
    let dir = data_dir("second");
    let Ok(Claim::First(first)) = claim(&dir) else {
        panic!("the first claim must win");
    };
    let (shown, requests) = mpsc::channel();
    let _first = first.serve(move |request| {
        let _ = shown.send(request);
    });
    let Ok(Claim::Running { delivered }) = claim(&dir) else {
        panic!("the second claim must find the first");
    };
    if cfg!(windows) {
        assert!(delivered);
        assert!(requests.recv_timeout(Duration::from_secs(5)).is_ok());
    }
}

#[test]
fn rf_ses_14_different_data_dirs_run_side_by_side() {
    let a = claim(&data_dir("side-a"));
    let b = claim(&data_dir("side-b"));
    assert!(matches!(a, Ok(Claim::First(_))));
    assert!(matches!(b, Ok(Claim::First(_))));
}

#[test]
fn rf_ses_14_a_claim_given_up_can_be_taken_again() {
    let dir = data_dir("again");
    let first = claim(&dir);
    assert!(matches!(first, Ok(Claim::First(_))));
    drop(first);
    assert!(matches!(claim(&dir), Ok(Claim::First(_))));
}
