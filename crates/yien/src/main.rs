//! The YIEN desktop app. For now it opens the window on sample data.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod demo;

use std::process::ExitCode;

fn main() -> ExitCode {
    match demo::workspace() {
        Ok(workspace) => {
            yien_ui::run(workspace);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("yien: the sample workspace is invalid: {error}");
            ExitCode::FAILURE
        }
    }
}
