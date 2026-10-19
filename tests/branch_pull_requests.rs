use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use buf_review::github;
use serde_json::{Value, json};
use tempfile::tempdir;

fn git(project: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(project)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run(case: &str) {
    let directory = tempdir().unwrap();
    let project = directory.path().join("project");
    fs::create_dir(&project).unwrap();
    git(&project, &["init", "--quiet", "-b", "local-feature"]);
    git(
        &project,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Base",
        ],
    );
    git(&project, &["config", "push.default", "simple"]);
    git(
        &project,
        &["remote", "add", "origin", "git@github.com:Fork/Repo.git"],
    );
    git(
        &project,
        &[
            "remote",
            "add",
            "upstream",
            "https://github.com/base/repo.git",
        ],
    );
    git(
        &project,
        &["config", "branch.local-feature.remote", "origin"],
    );
    git(
        &project,
        &[
            "config",
            "branch.local-feature.merge",
            "refs/heads/remote-feature",
        ],
    );
    match case {
        "tracking" | "zero" | "unresolved_push_url" => {}
        "detached" => git(&project, &["checkout", "--quiet", "--detach"]),
        "missing" => {
            git(
                &project,
                &["config", "--unset", "branch.local-feature.remote"],
            );
            git(
                &project,
                &["config", "--unset", "branch.local-feature.merge"],
            );
        }
        _ => {
            git(
                &project,
                &["config", "branch.local-feature.remote", "upstream"],
            );
            git(&project, &["config", "remote.pushDefault", "origin"]);
            git(&project, &["config", "push.default", "current"]);
        }
    }
    match case {
        "branch_push_remote" => {
            git(&project, &["config", "remote.pushDefault", "upstream"]);
            git(
                &project,
                &["config", "branch.local-feature.pushRemote", "origin"],
            );
        }
        "refspec" => {
            git(
                &project,
                &[
                    "config",
                    "remote.origin.push",
                    "refs/heads/local-feature:refs/heads/published-feature",
                ],
            );
            git(
                &project,
                &[
                    "remote",
                    "set-url",
                    "--push",
                    "origin",
                    "ssh://git@github.com:443/Fork/Repo.git",
                ],
            );
        }
        "namespace" => git(
            &project,
            &[
                "config",
                "remote.origin.fetch",
                "+refs/heads/*:refs/cache/origin/*",
            ],
        ),
        "exact_fetch" => git(
            &project,
            &[
                "config",
                "remote.origin.fetch",
                "+refs/heads/local-feature:refs/cache/one",
            ],
        ),
        "ambiguous_fetch" => {
            git(
                &project,
                &[
                    "config",
                    "remote.origin.fetch",
                    "+refs/heads/*:refs/cache/*",
                ],
            );
            git(
                &project,
                &[
                    "config",
                    "--add",
                    "remote.origin.fetch",
                    "+refs/heads/prefix/*:refs/cache/*",
                ],
            );
        }
        "unresolved" => git(
            &project,
            &[
                "config",
                "branch.local-feature.pushRemote",
                "missing-remote",
            ],
        ),
        "unresolved_ref" => git(
            &project,
            &[
                "config",
                "remote.origin.push",
                "refs/heads/other:refs/heads/published-feature",
            ],
        ),
        "foreign_host" => git(
            &project,
            &[
                "remote",
                "set-url",
                "--push",
                "origin",
                "ssh://git@example.invalid/fork/repo.git",
            ],
        ),
        "unresolved_push_url" => git(
            &project,
            &[
                "remote",
                "set-url",
                "--push",
                "origin",
                "ssh://git@example.invalid/fork/repo.git",
            ],
        ),
        _ => {}
    }
    let pull = |number: u64, branch: &str, repository: &str, updated: &str| {
        json!({
            "number":number, "title":format!("PR {number}"), "html_url":format!("https://github.com/base/repo/pull/{number}"),
            "user":{"id":1,"login":"author"}, "assignees":[], "state":"open", "merged_at":null, "updated_at":updated,
            "head":{"ref":branch,"repo":{"full_name":repository}}
        })
    };
    let mut closed = pull(6, "remote-feature", "fork/repo", "2026-01-09");
    closed["state"] = json!("closed");
    let mut deleted_fork = pull(7, "remote-feature", "fork/repo", "2026-01-09");
    deleted_fork["head"]["repo"] = Value::Null;
    let pages = if case == "zero" {
        json!([[]])
    } else {
        json!([
            [
                pull(1, "remote-feature", "fork/repo", "2026-01-01"),
                pull(5, "remote-feature", "other/repo", "2026-01-09"),
                closed
            ],
            [
                pull(2, "remote-feature", "FORK/REPO", "2026-01-02"),
                pull(3, "local-feature", "fork/repo", "2026-01-03"),
                pull(4, "published-feature", "fork/repo", "2026-01-04"),
                deleted_fork
            ]
        ])
    };
    fs::write(
        directory.path().join("pages.json"),
        serde_json::to_vec(&pages).unwrap(),
    )
    .unwrap();
    let executable = directory.path().join("gh");
    fs::write(&executable, r#"#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['BRANCH_ROOT'])
with (root / 'requests.jsonl').open('a') as stream:
    stream.write(json.dumps({'args':sys.argv[1:], 'cwd':os.getcwd()}) + '\n')
case = os.environ['BRANCH_CASE']
if sys.argv[1] == 'repo':
    assert pathlib.Path.cwd() == root / 'project'
    if case == 'repo_error':
        print('fixture repository unavailable', file=sys.stderr)
        sys.exit(1)
    print(json.dumps({'nameWithOwner':'base/repo'}))
else:
    assert sys.argv[1] == 'api'
    assert sys.argv[2] == 'repos/base/repo/pulls?state=open&sort=updated&direction=desc&per_page=100'
    assert sys.argv[3:] == ['--hostname','github.com','--method','GET','--paginate','--slurp']
    if case == 'api_error':
        print('fixture provider unavailable', file=sys.stderr)
        sys.exit(1)
    print((root / 'pages.json').read_text())
"#).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(std::iter::once(directory.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "branch_lookup_child", "--nocapture"])
        .env("BRANCH_CASE", case)
        .env("BRANCH_ROOT", directory.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{case}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn source_branch_lookup_preserves_zero_and_multiple_matches_and_filters_exact_head_identity() {
    for case in [
        "tracking",
        "zero",
        "push",
        "branch_push_remote",
        "refspec",
        "namespace",
        "exact_fetch",
    ] {
        run(case);
    }
}

#[test]
fn unresolved_branch_identity_falls_back_without_contacting_provider() {
    for case in [
        "detached",
        "missing",
        "unresolved",
        "unresolved_ref",
        "foreign_host",
        "unresolved_push_url",
        "ambiguous_fetch",
    ] {
        run(case);
    }
}

#[test]
fn provider_failures_are_distinct_from_no_matching_pull_request() {
    for case in ["repo_error", "api_error"] {
        run(case);
    }
}

#[test]
fn branch_lookup_child() {
    let Ok(case) = std::env::var("BRANCH_CASE") else {
        return;
    };
    let directory = std::env::var("BRANCH_ROOT").unwrap();
    let root = Path::new(&directory);
    let rows = github::branch_pull_requests(&root.join("project"));
    if matches!(case.as_str(), "repo_error" | "api_error") {
        assert!(rows.unwrap_err().to_string().contains("unavailable"));
        return;
    }
    let expected = match case.as_str() {
        "tracking" => vec![2, 1],
        "push" | "branch_push_remote" | "namespace" | "exact_fetch" => vec![3],
        "refspec" => vec![4],
        _ => vec![],
    };
    assert_eq!(
        rows.unwrap()
            .iter()
            .map(|row| row.number)
            .collect::<Vec<_>>(),
        expected
    );
    if matches!(
        case.as_str(),
        "detached"
            | "missing"
            | "unresolved"
            | "unresolved_ref"
            | "foreign_host"
            | "unresolved_push_url"
            | "ambiguous_fetch"
    ) {
        assert!(
            !root.join("requests.jsonl").exists(),
            "unresolved identity must not guess a remote PR"
        );
    }
    assert!(
        !root.join("project/.review").exists(),
        "lookup must not create local review state"
    );
}
