use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output, Stdio},
};
use tempfile::{TempDir, tempdir};

struct Fixture {
    directory: TempDir,
    base: String,
    head: String,
}

fn git(project: &Path, args: &[&str]) -> String {
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
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

impl Fixture {
    fn new() -> Self {
        let directory = tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "-q", "-b", "master"]);
        fs::write(root.join("file"), "base\n").unwrap();
        git(root, &["add", "file"]);
        git(
            root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "-qm",
                "base",
            ],
        );
        let base = git(root, &["rev-parse", "HEAD"]);
        git(root, &["switch", "-qc", "feature"]);
        fs::write(root.join("file"), "base\ncommitted\n").unwrap();
        git(root, &["add", "file"]);
        git(
            root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "-qm",
                "feature",
            ],
        );
        let head = git(root, &["rev-parse", "HEAD"]);
        git(
            root,
            &["remote", "add", "origin", "git@github.com:fork/repo.git"],
        );
        git(
            root,
            &[
                "remote",
                "set-url",
                "--push",
                "origin",
                "git@github.com:fork/repo.git",
            ],
        );
        git(
            root,
            &["remote", "set-url", "origin", root.to_str().unwrap()],
        );
        git(root, &["config", "push.default", "simple"]);
        fs::create_dir(root.join("bin")).unwrap();
        let executable = root.join("bin/gh");
        fs::write(&executable, r#"#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
root = pathlib.Path(os.environ['INTENT_ROOT'])
state_path = root / 'provider.json'
state = json.loads(state_path.read_text())
args = sys.argv[1:]
endpoint = args[1] if len(args)>1 else ''
method = args[args.index('--method')+1] if '--method' in args else 'GET'
body = json.load(sys.stdin) if '--input' in args else None
with (root / 'requests.jsonl').open('a') as out:
 out.write(json.dumps({'args':args,'method':method,'body':body})+'\n')
def save(): state_path.write_text(json.dumps(state))
def pull():
 return {'id':70,'number':7,'html_url':'https://github.com/target/repo/pull/7','state':'open','title':state.get('created_title','Remote title'),'body':state.get('created_body','Remote body'),'head':{'ref':'feature','repo':{'full_name':'fork/repo'},'sha':state['head']},'base':{'ref':'master','repo':{'full_name':'target/repo'},'sha':state['base']}}
def connection(nodes=[]):
 return {'totalCount':len(nodes),'nodes':nodes,'pageInfo':{'hasNextPage':False,'endCursor':None}}
if args[0]=='repo': print(json.dumps({'nameWithOwner':'target/repo'}))
elif endpoint=='user': print(json.dumps({'id':state.get('user',7),'login':'reviewer'}))
elif endpoint.startswith('repos/target/repo/pulls?'):
 if state.get('lookup_fail'): print('HTTP 503 lookup unavailable',file=sys.stderr);sys.exit(1)
 print(json.dumps([state['pulls']]))
elif endpoint.startswith('repos/fork/repo/branches/'):
 if state.get('missing'):
  print('HTTP 404',file=sys.stderr);sys.exit(1)
 if state.get('checkout_head'):
  subprocess.run(['git','-C',str(root),'checkout','--quiet','--detach',state['checkout_head']],check=True)
 print(json.dumps({'commit':{'sha':state['head']}}))
elif endpoint=='repos/target/repo/pulls' and method=='POST':
 mode=state.get('mode','')
 if mode=='rejected_lookup_failure':
  state['lookup_fail']=True;save();print('HTTP 403',file=sys.stderr);sys.exit(1)
 if mode=='rejected': print('HTTP 403',file=sys.stderr);sys.exit(1)
 state['created_title']=body['title'];state['created_body']=body['body']
 if mode!='uncertain_absent': state['pulls']=[pull()]
 save()
 if mode in ('uncertain','uncertain_absent'): print('HTTP 502',file=sys.stderr);sys.exit(1)
 if mode=='race': print('HTTP 422',file=sys.stderr);sys.exit(1)
 print(json.dumps(pull()))
elif endpoint=='graphql':
 assert 'ReviewSnapshot' in body['query'],body
 user={'login':'reviewer','databaseId':7}
 pr={'id':'PR_70','fullDatabaseId':'70','title':'Remote title','body':'Remote body','author':user,'state':'OPEN','baseRefOid':state['base'],'headRefOid':state['head'],'headRefName':'feature','baseRefName':'master','headRepository':{'url':'https://github.com/fork/repo'},'changedFiles':1,'files':connection([{'path':'file','changeType':'MODIFIED','viewerViewedState':'UNVIEWED'}]),'comments':connection(),'reviews':connection(),'reviewThreads':connection()}
 print(json.dumps({'data':{'viewer':user,'repository':{'pullRequest':pr}}}))
elif endpoint=='repos/target/repo/pulls/7/files?per_page=100':
 print(json.dumps([[{'filename':'file','status':'modified','additions':1,'deletions':0}]]))
elif endpoint=='repos/target/repo/pulls/7':
 print(json.dumps({'base':{'sha':state['base']},'head':{'sha':state['head']},'changed_files':1,'node_id':'PR_70'}))
elif endpoint=='repos/target/repo/issues/7/comments' and method=='POST':
 state['comment_id']=state.get('comment_id',100)+1;save();print(json.dumps({'id':state['comment_id']}))
else:
 print('Unexpected request: '+repr(args),file=sys.stderr);sys.exit(1)
"#).unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        let fixture = Self {
            directory,
            base,
            head,
        };
        fixture.state(json!({"base":fixture.base,"head":fixture.head,"pulls":[]}));
        fixture
    }
    fn root(&self) -> &Path {
        self.directory.path()
    }
    fn state(&self, state: Value) {
        fs::write(
            self.root().join("provider.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
    }
    fn change(&self, key: &str, value: Value) {
        let mut state: Value =
            serde_json::from_slice(&fs::read(self.root().join("provider.json")).unwrap()).unwrap();
        state[key] = value;
        self.state(state);
    }
    fn command(&self, request: Value) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_pr"))
            .arg("workspace")
            .current_dir(self.root())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root().join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("INTENT_ROOT", self.root())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&request).unwrap())
            .unwrap();
        child.wait_with_output().unwrap()
    }
    fn call(&self, request: Value) -> Value {
        let output = self.command(request);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn open(&self) -> Value {
        let route = self.call(json!({"op":"pr","base":"master"}));
        assert_eq!(route["route"], "batch");
        self.call(json!({"op":"select","batch":route["batch"]}))["workspace"].clone()
    }
    fn details(&self, view: &Value) -> Value {
        self.call(json!({"op":"actions","workspace":view["id"],"generation":view["generation"],"actions":[{"action":"pr_details","revision":view["pr"]["revision"],"title":"Local title λ","body":"Body\nsecond line"}]}))["workspace"].clone()
    }
    fn publish(&self, view: &Value) -> Value {
        self.call(json!({"op":"publish","workspace":view["id"],"generation":view["generation"]}))
    }
    fn requests(&self) -> Vec<Value> {
        fs::read_to_string(self.root().join("requests.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn pull(&self) -> Value {
        json!({"id":70,"number":7,"html_url":"https://github.com/target/repo/pull/7","state":"open","title":"Remote title","body":"Remote body","head":{"ref":"feature","repo":{"full_name":"fork/repo"},"sha":self.head},"base":{"ref":"master","repo":{"full_name":"target/repo"},"sha":self.base}})
    }
}

#[test]
fn local_entry_is_committed_only_resumes_and_refreshes_saved_branch() {
    let fixture = Fixture::new();
    fs::write(fixture.root().join("file"), "base\ncommitted\nstaged\n").unwrap();
    git(fixture.root(), &["add", "file"]);
    fs::write(
        fixture.root().join("file"),
        "base\ncommitted\nstaged\nunstaged\n",
    )
    .unwrap();
    fs::write(fixture.root().join("untracked"), "ignored by review\n").unwrap();
    let view = fixture.open();
    assert_eq!(view["pr"]["source"]["kind"], "local_intent");
    assert_eq!(view["review"]["read"]["pull_request"]["status"], "local");
    assert_eq!(view["comparison"]["head"], fixture.head);
    assert_eq!(view["files"].as_array().unwrap().len(), 1);
    assert_eq!(view["files"][0]["stats"]["additions"], 1);
    let code = fixture.call(json!({"op":"code","workspace":view["id"],"path":"file"}));
    assert_eq!(code["code"]["head_content"], "base\ncommitted\n");
    let again = fixture.open();
    assert_eq!(again["id"], view["id"]);
    assert_eq!(again["pr"]["id"], view["pr"]["id"]);
    git(fixture.root(), &["reset", "--hard", "HEAD"]);
    git(fixture.root(), &["switch", "-q", "master"]);
    let refreshed = fixture
        .call(json!({"op":"refresh","workspace":view["id"],"generation":view["generation"]}));
    assert!(refreshed["error"].is_null(), "{refreshed}");
    assert_eq!(refreshed["workspace"]["comparison"]["head"], fixture.base);
    assert_eq!(
        refreshed["workspace"]["review"]["read"]["pull_request"]["revision"]["head"],
        fixture.head
    );
    assert_eq!(git(fixture.root(), &["branch", "--show-current"]), "master");
}

#[test]
fn creation_with_no_comments_is_draft_and_keeps_all_local_ids() {
    let fixture = Fixture::new();
    let view = fixture.details(&fixture.open());
    let response = fixture.publish(&view);
    assert!(response["error"].is_null(), "{response}");
    let published = &response["workspace"];
    assert_eq!(published["id"], view["id"]);
    assert_eq!(published["pr"]["id"], view["pr"]["id"]);
    assert_eq!(published["review"]["id"], view["review"]["id"]);
    assert_eq!(published["comparison"], view["comparison"]);
    assert_eq!(published["pr"]["source"]["kind"], "remote");
    assert_eq!(
        published["locator"],
        "https://github.com/target/repo/pull/7"
    );
    let requests = fixture.requests();
    let create = requests
        .iter()
        .find(|request| {
            request["args"][1] == "repos/target/repo/pulls" && request["method"] == "POST"
        })
        .unwrap();
    assert_eq!(
        create["body"],
        json!({"title":"Local title λ","body":"Body\nsecond line","head":"fork:feature","head_repo":"repo","base":"master","draft":true})
    );
    let repeated = fixture.publish(published);
    assert!(repeated["error"].is_null(), "{repeated}");
    assert_eq!(
        fixture
            .requests()
            .iter()
            .filter(|request| request["args"][1] == "repos/target/repo/pulls"
                && request["method"] == "POST")
            .count(),
        1
    );
}

#[test]
fn existing_exact_pair_is_reused_without_overwriting_metadata() {
    let fixture = Fixture::new();
    let local = fixture.open();
    let mut other = fixture.pull();
    other["head"]["repo"]["full_name"] = json!("other/repo");
    fixture.change("pulls", json!([other, fixture.pull()]));
    let response = fixture.publish(&local);
    assert!(response["error"].is_null(), "{response}");
    assert_eq!(response["workspace"]["pr"]["id"], local["pr"]["id"]);
    assert_eq!(response["workspace"]["pr"]["title"], "Remote title");
    assert_eq!(response["workspace"]["pr"]["body"], "Remote body");
    assert!(
        !fixture
            .requests()
            .iter()
            .any(|request| request["method"] == "POST")
    );
}

#[test]
fn existing_remote_entry_uses_remote_pair_and_routes_without_creation() {
    let fixture = Fixture::new();
    fixture.change("pulls", json!([fixture.pull()]));
    let view = fixture.open();
    assert_eq!(view["pr"]["source"]["kind"], "remote");
    assert_eq!(view["pr"]["source"]["branches"]["sink"]["branch"], "master");
    assert_eq!(view["pr"]["title"], "Remote title");
    assert!(
        !fixture
            .requests()
            .iter()
            .any(|request| request["args"][1] == "repos/target/repo/pulls"
                && request["method"] == "POST")
    );
}

#[test]
fn uncertain_create_reconciles_without_second_write_or_blocks_if_missing() {
    for mode in ["uncertain", "uncertain_absent", "race"] {
        let fixture = Fixture::new();
        let view = fixture.details(&fixture.open());
        fixture.change("mode", json!(mode));
        let first = fixture.publish(&view);
        if mode == "race" {
            assert!(first["error"].is_null(), "{first}");
        } else {
            assert!(
                first["error"].as_str().unwrap().contains("HTTP 502"),
                "{first}"
            );
        }
        let second = fixture.publish(&first["workspace"]);
        if mode == "uncertain_absent" {
            assert!(
                second["error"].as_str().unwrap().contains("uncertain"),
                "{second}"
            );
        } else {
            assert!(second["error"].is_null(), "{second}");
        }
        assert_eq!(
            fixture
                .requests()
                .iter()
                .filter(|request| request["args"][1] == "repos/target/repo/pulls"
                    && request["method"] == "POST")
                .count(),
            1
        );
    }
}

#[test]
fn missing_title_unpushed_commits_wrong_user_and_rejections_keep_intent() {
    for case in ["title", "missing", "unpushed", "user", "rejected"] {
        let fixture = Fixture::new();
        let initial = fixture.open();
        let view = if case == "title" {
            initial
        } else {
            fixture.details(&initial)
        };
        match case {
            "missing" => fixture.change("missing", json!(true)),
            "unpushed" => fixture.change("head", json!(fixture.base)),
            "user" => fixture.change("user", json!(99)),
            "rejected" => fixture.change("mode", json!("rejected")),
            _ => {}
        }
        let response = fixture.publish(&view);
        let error = response["error"].as_str().unwrap();
        let expected = match case {
            "title" => "title",
            "missing" => "push",
            "unpushed" => "not at remote head",
            "user" => "user",
            _ => "HTTP 403",
        };
        assert!(error.contains(expected), "{case}: {response}");
        assert_eq!(
            response["workspace"]["pr"]["source"]["kind"],
            "local_intent"
        );
        if case == "rejected" {
            fixture.change("mode", json!(""));
            let retry = fixture.publish(&response["workspace"]);
            assert!(retry["error"].is_null(), "{retry}");
        }
    }
}

#[test]
fn publication_binds_every_batch_before_sending_comments() {
    let fixture = Fixture::new();
    let first = fixture.details(&fixture.open());
    let second = fixture.call(json!({"op":"new","workspace":first["id"]}))["workspace"].clone();
    let save = |view: &Value, body: &str| {
        fixture.call(json!({
        "op":"actions","workspace":view["id"],"generation":view["generation"],
        "actions":[{"action":"save","body":body,"target":{"kind":"pull_request"},"destination":"standalone"}]
    }))["workspace"].clone()
    };
    let first = save(&first, "first batch comment");
    let second = save(&second, "second batch comment");
    let first_comment = first["saved_comment"].clone();
    let second_comment = second["saved_comment"].clone();
    let published = fixture.publish(&first);
    assert!(published["error"].is_null(), "{published}");
    let other = fixture.call(json!({"op":"get","workspace":second["id"]}))["workspace"].clone();
    assert_eq!(other["pr"]["source"]["kind"], "remote");
    assert_eq!(other["comparison"], second["comparison"]);
    assert_eq!(other["saved_comment"], second_comment);
    let published_other = fixture.publish(&other);
    assert!(published_other["error"].is_null(), "{published_other}");
    assert_eq!(
        published["workspace"]["comments"][0]["comment"]["comment"]["id"],
        first_comment
    );
    let requests = fixture.requests();
    let writes = requests
        .iter()
        .filter(|request| request["method"] == "POST")
        .map(|request| request["args"][1].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        writes,
        [
            "repos/target/repo/pulls",
            "repos/target/repo/issues/7/comments",
            "repos/target/repo/issues/7/comments"
        ]
    );
}

#[test]
fn custom_push_ref_and_switched_head_refresh_use_saved_source() {
    let fixture = Fixture::new();
    git(
        fixture.root(),
        &["config", "branch.feature.remote", "origin"],
    );
    git(
        fixture.root(),
        &["config", "branch.feature.merge", "refs/heads/feature"],
    );
    git(
        fixture.root(),
        &[
            "config",
            "remote.origin.push",
            "refs/heads/feature:refs/heads/published",
        ],
    );
    let view = fixture.open();
    assert_eq!(
        view["pr"]["source"]["branches"]["source"]["branch"],
        "published"
    );
    git(fixture.root(), &["switch", "-q", "master"]);
    let refreshed = fixture
        .call(json!({"op":"refresh","workspace":view["id"],"generation":view["generation"]}));
    assert!(refreshed["error"].is_null(), "{refreshed}");
    assert_eq!(refreshed["workspace"]["comparison"]["head"], fixture.base);
    assert_eq!(
        refreshed["workspace"]["review"]["read"]["pull_request"]["revision"]["head"],
        fixture.head
    );
    assert_eq!(git(fixture.root(), &["branch", "--show-current"]), "master");
}

#[test]
fn invalid_or_disabled_push_destinations_are_not_guessed() {
    for mode in ["nothing", "matching", "upstream"] {
        let fixture = Fixture::new();
        git(fixture.root(), &["config", "push.default", mode]);
        let output = fixture.command(json!({"op":"pr","base":"master"}));
        assert!(!output.status.success(), "{mode}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("push destination"));
    }
}

#[test]
fn reused_remote_pr_also_requires_reviewed_commits_pushed() {
    let fixture = Fixture::new();
    let view = fixture.open();
    let mut pull = fixture.pull();
    pull["head"]["sha"] = json!(fixture.base);
    fixture.change("pulls", json!([pull]));
    let response = fixture.publish(&view);
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("not at remote head"),
        "{response}"
    );
    assert_eq!(
        response["workspace"]["pr"]["source"]["kind"],
        "local_intent"
    );
}

#[test]
fn checkout_change_during_source_lookup_cannot_create_a_pr() {
    let fixture = Fixture::new();
    let view = fixture.details(&fixture.open());
    fs::write(fixture.root().join("file"), "replacement checkout\n").unwrap();
    git(
        fixture.root(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "--quiet",
            "-am",
            "Replacement",
        ],
    );
    let replacement = git(fixture.root(), &["rev-parse", "HEAD"]);
    git(fixture.root(), &["reset", "--hard", &fixture.head]);
    fixture.change("checkout_head", json!(replacement));
    let response = fixture.publish(&view);
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("Git HEAD changed while preparing publication"),
        "{response}"
    );
    assert_eq!(
        response["workspace"]["pr"]["source"]["kind"],
        "local_intent"
    );
    assert_eq!(response["workspace"]["generation"], view["generation"]);
    assert!(response["workspace"]["attempt"].is_null());
    assert!(
        !fixture
            .requests()
            .iter()
            .any(|request| request["method"] == "POST")
    );
    fixture.change("checkout_head", Value::Null);
    git(fixture.root(), &["checkout", "--quiet", "feature"]);
    let retry = fixture.publish(&response["workspace"]);
    assert!(retry["error"].is_null(), "{retry}");
}

#[test]
fn newer_remote_source_head_is_fetched_and_blocks_publication_without_checkout() {
    let fixture = Fixture::new();
    let view = fixture.details(&fixture.open());
    let clone = tempdir().unwrap();
    git(
        clone.path(),
        &["clone", "-q", fixture.root().to_str().unwrap(), "."],
    );
    fs::write(clone.path().join("file"), "base\ncommitted\nremote later\n").unwrap();
    git(clone.path(), &["add", "file"]);
    git(
        clone.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "later",
        ],
    );
    let remote_head = git(clone.path(), &["rev-parse", "HEAD"]);
    fixture.change("head", json!(remote_head));
    git(
        fixture.root(),
        &[
            "remote",
            "set-url",
            "origin",
            clone.path().to_str().unwrap(),
        ],
    );
    git(
        fixture.root(),
        &[
            "config",
            &format!("url.{}.insteadOf", clone.path().display()),
            "https://github.com/fork/repo",
        ],
    );
    let branches = git(fixture.root(), &["show-ref", "--heads"]);
    let response = fixture.publish(&view);
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("not at remote head"),
        "{response}"
    );
    assert_eq!(git(fixture.root(), &["show-ref", "--heads"]), branches);
    assert_eq!(
        git(
            fixture.root(),
            &["rev-parse", "refs/remotes/origin/feature"]
        ),
        remote_head
    );
    assert_eq!(git(fixture.root(), &["rev-parse", "HEAD"]), fixture.head);
    assert_eq!(
        fs::read_to_string(fixture.root().join("file")).unwrap(),
        "base\ncommitted\n"
    );
    assert_eq!(response["workspace"]["comparison"], view["comparison"]);
    assert!(
        !fixture
            .requests()
            .iter()
            .any(|request| request["method"] == "POST")
    );
}

#[test]
fn confirmed_rejection_is_retryable_even_if_reconciliation_read_fails() {
    let fixture = Fixture::new();
    let view = fixture.details(&fixture.open());
    fixture.change("mode", json!("rejected_lookup_failure"));
    let failed = fixture.publish(&view);
    assert!(
        failed["error"]
            .as_str()
            .unwrap()
            .contains("lookup unavailable"),
        "{failed}"
    );
    fixture.change("lookup_fail", json!(false));
    fixture.change("mode", json!(""));
    let edited = fixture.call(json!({"op":"actions","workspace":failed["workspace"]["id"],"generation":failed["workspace"]["generation"],"actions":[{"action":"pr_details","revision":failed["workspace"]["pr"]["revision"],"title":"Retry title","body":"Revised body"}]}))["workspace"].clone();
    assert!(
        edited["pr"]["revision"].as_u64().unwrap()
            > failed["workspace"]["pr"]["revision"].as_u64().unwrap()
    );
    let retry = fixture.publish(&edited);
    assert!(retry["error"].is_null(), "{retry}");
}

#[test]
fn second_reviewer_publishes_shared_local_pr_with_own_provider_identity() {
    let fixture = Fixture::new();
    let first = fixture.details(&fixture.open());
    fixture.change("user", json!(8));
    let second = fixture.open();
    assert_ne!(second["id"], first["id"]);
    assert_eq!(second["pr"]["id"], first["pr"]["id"]);
    let second = fixture.call(json!({"op":"actions","workspace":second["id"],"generation":second["generation"],"actions":[{"action":"save","body":"Second reviewer's comment","target":{"kind":"pull_request"},"destination":"standalone"}]}))["workspace"].clone();
    fixture.change("user", json!(7));
    let first_published = fixture.publish(&first);
    assert!(first_published["error"].is_null(), "{first_published}");
    fixture.change("user", json!(8));
    let second_published = fixture.publish(&second);
    assert!(second_published["error"].is_null(), "{second_published}");
    assert_eq!(second_published["workspace"]["pr"]["id"], first["pr"]["id"]);
    assert_eq!(
        second_published["workspace"]["comparison"],
        second["comparison"]
    );
    assert!(
        second_published["workspace"]["pending"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let store = buf_review::store::Store::open(fixture.root()).unwrap();
    assert_eq!(
        store
            .workspace(second["id"].as_str().unwrap())
            .unwrap()
            .unwrap()
            .review
            .read
            .pull_request
            .status,
        buf_review::model::PullRequestStatus::Local
    );
}
