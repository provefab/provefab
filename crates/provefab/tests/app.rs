//! The library entry point Provefab Pro builds on (spec §3, D59).

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use provefab::app::{Extensions, Extra, cli_command, run_from};

static RAN: AtomicBool = AtomicBool::new(false);

fn hello(_: &clap::ArgMatches) -> ExitCode {
    RAN.store(true, Ordering::SeqCst);
    ExitCode::SUCCESS
}

#[test]
fn an_extra_command_is_routed_to_its_handler() {
    let ext = Extensions {
        name: "provefab-pro",
        commands: vec![Extra {
            command: clap::Command::new("hello"),
            run: hello,
        }],
        ..Extensions::default()
    };
    assert_eq!(cli_command(&ext).get_name(), "provefab-pro");
    let _ = run_from(ext, ["provefab-pro", "hello"]);
    assert!(RAN.load(Ordering::SeqCst));
}

/// BYOK: `provefab login <worker> --api-key` switches that worker's key login.
#[test]
fn login_accepts_an_api_key_flag() {
    let cmd = cli_command(&Extensions::default());
    for worker in ["claude", "codex"] {
        assert!(
            cmd.clone()
                .try_get_matches_from(["provefab", "login", worker, "--api-key"])
                .is_ok(),
            "{worker}"
        );
    }
}
