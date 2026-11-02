//! Shutdown-signal test: a real SIGTERM to this process resolves the helper.

use crate::signal::shutdown_signal;

#[test]
fn terminate_resolves_the_helper() {
    common_core::runtime::block_on(async {
        let pid = std::process::id();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let _ = std::process::Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status();
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), shutdown_signal())
            .await
            .expect("SIGTERM must resolve the helper");
    });
}
