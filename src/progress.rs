//! Ephemeral CLI progress. State and final responses do not depend on delivery.
use serde::Serialize;
use std::io::{self, Write};

#[derive(Serialize)]
struct Progress<'a> {
    event: &'static str,
    stage: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<usize>,
}

pub(crate) fn report(stage: &str, message: &str, counts: Option<(usize, usize)>) {
    let progress = Progress {
        event: "progress",
        stage,
        message,
        completed: counts.map(|(completed, _)| completed),
        total: counts.map(|(_, total)| total),
    };
    let mut stderr = io::stderr().lock();
    // A closed progress pipe must not interrupt a provider write or its receipt.
    let _ = serde_json::to_writer(&mut stderr, &progress);
    let _ = writeln!(stderr);
    let _ = stderr.flush();
}
