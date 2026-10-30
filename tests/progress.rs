use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use serde_json::{Value, json};
use tempfile::tempdir;

#[test]
fn progress_arrives_before_completion_without_changing_stdout_or_diagnostics() {
    for failure in [false, true] {
        let project = tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(project.path())
                .status()
                .unwrap()
                .success()
        );
        let bin = project.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let gh = bin.join("gh");
        fs::write(
            &gh,
            r#"#!/bin/sh
if [ "$1" = repo ]; then
  progress_wait=0
  while [ ! -f "$PROGRESS_RELEASE" ]; do
    progress_wait=$((progress_wait + 1))
    if [ "$progress_wait" -ge 500 ]; then exit 1; fi
    sleep 0.01
  done
  if [ "$PROGRESS_FAIL" = yes ]; then printf 'provider diagnostic retained\n' >&2; exit 1; fi
  printf '{"nameWithOwner":"owner/repo"}'
else
  printf '[[]]'
fi
"#,
        )
        .unwrap();
        fs::set_permissions(gh, fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))
        .unwrap();
        let release = project.path().join("release");
        let mut child = Command::new(env!("CARGO_BIN_EXE_pr"))
            .arg("workspace")
            .current_dir(project.path())
            .env("PATH", path)
            .env("PROGRESS_RELEASE", &release)
            .env("PROGRESS_FAIL", if failure { "yes" } else { "no" })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"op":"list"}"#)
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let (sent, received) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut stderr = BufReader::new(stderr);
            let mut line = String::new();
            stderr.read_line(&mut line).unwrap();
            sent.send(line).unwrap();
            let mut remaining = String::new();
            stderr.read_to_string(&mut remaining).unwrap();
            remaining
        });
        let event = received.recv_timeout(Duration::from_secs(5));
        if event.is_err() {
            let _ = child.kill();
        }
        let event: Value =
            serde_json::from_str(&event.expect("progress was buffered until command completion"))
                .unwrap();
        assert_eq!(event["event"], "progress");
        assert_eq!(event["stage"], "fetch_prs");
        assert!(event["message"].is_string());
        assert!(event.get("completed").is_none() && event.get("total").is_none());
        assert!(
            child.try_wait().unwrap().is_none(),
            "command finished before its blocked provider call"
        );
        fs::write(release, "").unwrap();
        let result = child.wait_with_output().unwrap();
        let diagnostics = reader.join().unwrap();
        if failure {
            assert!(!result.status.success());
            assert!(result.stdout.is_empty());
            assert!(diagnostics.contains("provider diagnostic retained"));
        } else {
            assert!(result.status.success(), "{diagnostics}");
            assert_eq!(
                serde_json::from_slice::<Value>(&result.stdout).unwrap(),
                json!({"pull_requests":[]})
            );
            assert!(diagnostics.is_empty(), "{diagnostics}");
        }
    }
}
