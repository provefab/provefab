//! SIGTERM is process-wide: keep this the only test in this binary.
#![cfg(unix)]

use std::time::Duration;

#[tokio::test]
async fn sigterm_completes_the_shutdown_future() {
    let stop = tokio::spawn(provefab::app::shutdown_signal());
    tokio::task::yield_now().await;
    // SAFETY: raising a signal whose handler is already registered.
    assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
    tokio::time::timeout(Duration::from_secs(5), stop)
        .await
        .expect("shutdown future did not complete on SIGTERM")
        .unwrap();
}
