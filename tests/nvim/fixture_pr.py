#!/usr/bin/env python3
"""Deterministic workspace protocol fixture; no network or provider writes."""
import atexit
import copy
import json
import os
import pathlib
import sys
import time

directory = pathlib.Path(os.environ["BUF_REVIEW_FIXTURE"])
publication_case_path = directory / "post_publish.json"
publication_case = json.loads(publication_case_path.read_text()) if publication_case_path.exists() else None
publication_applied = False


def publication_event(phase, **fields):
    if publication_case:
        with (directory / "post_publish_events.jsonl").open("a") as stream:
            stream.write(json.dumps({"phase": phase, **fields}) + "\n")


def write_json(path, value):
    temporary = path.with_name(path.name + "." + str(os.getpid()))
    temporary.write_text(json.dumps(value))
    temporary.replace(path)


def canonical_pr(view):
    for row in view["comments"]:
        if row["comment"]["origin"] == "draft":
            row["draft"] = bool(row.get("destination"))
        else:
            row["draft"] = row.get("draft", row["comment"]["comment"].get("draft", False))
    pull = view["review"]["read"]["pull_request"]
    view.setdefault("pr", {
        "id": pull["id"], "revision": 0,
        "source": {"kind": "remote", "locator": view.get("locator")},
        "title": pull["title"], "body": pull.get("body", ""),
    })
    return view


request = json.load(sys.stdin)
with (directory / "requests.jsonl").open("a") as stream:
    stream.write(json.dumps(request) + "\n")
if request["op"] == "fetch_refresh":
    selected = json.loads((directory / "state.json").read_text())
    if request["workspace"] != selected["id"]:
        selected = json.loads((directory / "views.json").read_text())[request["workspace"]]
    prepared = {"workspace": selected["id"], "generation": selected["generation"]}
    published_path = directory / "post_published.json"
    published = json.loads(published_path.read_text()) if published_path.exists() else None
    publication_event("fetch", publication=published and published["tag"])
    if published:
        prepared["fixture_snapshot"] = published["snapshot"]
    if (directory / "delay_refresh").exists():
        time.sleep(float((directory / "delay_refresh").read_text()))
    if (directory / "fail_refresh").exists():
        prepared["error"] = "Fixture refresh failed\r\nProvider diagnostic\n  Final diagnostic"
    else:
        prepared["review"] = selected["review"]["id"]
    with (directory / "refresh_complete.jsonl").open("a") as stream:
        stream.write(json.dumps(request) + "\n")
    print(json.dumps({"prepared_refresh": prepared}))
    sys.exit(0)
if request["op"] == "fetch_objects":
    selected = json.loads((directory / "state.json").read_text())
    if request["workspace"] != selected["id"]:
        selected = json.loads((directory / "views.json").read_text())[request["workspace"]]
    revision = selected["comparison"]
    if (directory / "delay_fetch_objects").exists():
        time.sleep(float((directory / "delay_fetch_objects").read_text()))
    failure = directory / "fail_fetch_objects"
    response = {"error": failure.read_text().strip()} if failure.exists() else {"revision": revision}
    with (directory / "fetch_objects_complete.jsonl").open("a") as stream:
        stream.write(json.dumps({"request": request, "response": response}) + "\n")
    print(json.dumps(response))
    sys.exit(0)
if request["op"] == "refresh":
    def completed_refresh():
        with (directory / "refresh_complete.jsonl").open("a") as stream:
            stream.write(json.dumps(request) + "\n")
    atexit.register(completed_refresh)
if request["op"] == "refresh" and (directory / "delay_refresh").exists():
    time.sleep(float((directory / "delay_refresh").read_text()))
if request["op"] != "refresh" and (directory / ("delay_" + request["op"])).exists():
    time.sleep(float((directory / ("delay_" + request["op"])).read_text()))
state_path = directory / "state.json"
view = canonical_pr(json.loads(state_path.read_text()))
active_id = view["id"]
views_path = directory / "views.json"
views = {key: canonical_pr(value) for key, value in json.loads(views_path.read_text()).items()} if views_path.exists() else {}
views[active_id] = view
if request.get("workspace") and request["workspace"] != active_id:
    view = views[request["workspace"]]
op = request["op"]
response = {}


def comment(row):
    return row["comment"]["comment"]


def row_for(identity):
    return next(row for row in view["comments"] + view.get("deleted_comments", []) if comment(row)["id"] == identity)


def mutate(request):
    global view, publication_applied
    op = request["op"]
    response = {}
    if op in {"undo", "delete_comments"}:
        if request.get("generation") != view["generation"]:
            return {"workspace": view, "error": "workspace changed in another session; reload before saving"}
        selected = list(dict.fromkeys(request["comments"]))
        operations = {}
        for identity in selected:
            row = row_for(identity)
            if row not in view["comments"]:
                continue
            if op == "delete_comments" or row.get("destination"):
                if comment(row)["author"]["id"] != view["review"]["reviewer"]["id"]:
                    return {"workspace": view, "error": "editing and deletion require verified ownership"}
                if row.get("summary"):
                    return {"workspace": view, "error": "Review summaries cannot be deleted"}
            if op == "delete_comments" and not (row["comment"]["origin"] == "draft" and row.get("destination")):
                return {"workspace": view, "error": "published comments are immutable"}
            if op == "delete_comments" or row.get("destination"):
                children = [comment(child)["id"] for child in view["comments"] if child.get("destination") and comment(child)["target"] == {"kind": "reply", "target": identity}]
                if any(child not in selected for child in children):
                    return {"workspace": view, "error": "Remove unpublished replies before deleting their parent"}
                operations[identity] = {"op": "delete", "comment": identity}
            elif row["fetched"]:
                operations[identity] = {"op": "reaction", "comment": identity, "present": False}
            else:
                return {"workspace": view, "error": "progress is available only for fetched comments"}
        ordered = []

        def append(identity):
            operation = operations.pop(identity, None)
            if operation is None:
                return
            if operation["op"] == "delete":
                for child in view["comments"]:
                    if comment(child)["target"] == {"kind": "reply", "target": identity}:
                        append(comment(child)["id"])
            ordered.append(operation)

        for identity in selected:
            append(identity)
        for operation in ordered:
            operation.update(workspace=request["workspace"], generation=view["generation"])
            with (directory / "action_attempts.jsonl").open("a") as stream:
                stream.write(json.dumps(operation) + "\n")
            response = mutate(operation)
            if response.get("error"):
                return response
        return {"workspace": view}
    if op in {"save", "delete"} and request.get("comment"):
        row = row_for(request["comment"])
        if not (row["comment"]["origin"] == "draft" and row.get("destination")):
            return {"workspace": view, "error": "published comments are immutable"}
    if op == "save" and (directory / "race_save").exists():
        view["generation"] += 1
        write_json(state_path, view)
    if op not in {"active", "open"} and request.get("generation") != view["generation"]:
        return {"workspace": view, "error": "Stale workspace generation" if op in {"refresh", "publish", "submit", "compare_latest"} else "workspace changed in another session; reload before saving"}
    previous = view
    view = copy.deepcopy(view)
    view["saved_comment"] = None
    if publication_case and op == publication_case["operation"]:
        view.update(copy.deepcopy(publication_case["local"]))
        publication_applied = True
        if publication_case.get("error"):
            response["error"] = publication_case["error"]
    elif op == "open" and (directory / "switch_workspace").exists():
        view["id"] = "workspace-42"
        view["locator"] = request["url"]
        view["review"]["read"]["pull_request"]["id"] = "PR-42"
        view["pr"].update(id="PR-42", source={"kind": "remote", "locator": request["url"]})
    elif op == "start":
        if not view.get("wip"):
            view["wip"] = {"id": "wip-1", "active": True, "conclusion": None}
        view["wip"]["active"] = True
    elif op == "leave":
        if view.get("wip"):
            view["wip"]["active"] = False
    elif op == "save":
        if request.get("comment"):
            row = row_for(request["comment"])
            if row["fetched"] or not row.get("destination"):
                response["error"] = "published comments are immutable"
            elif not request["body"].strip():
                if any(comment(child)["target"] == {"kind": "reply", "target": request["comment"]} and not child["fetched"] for child in view["comments"]):
                    response["error"] = "Remove local replies before deleting their parent"
                else:
                    view["comments"].remove(row)
                    view.setdefault("deleted_comments", []).append(row)
            else:
                if row not in view["comments"]:
                    view["deleted_comments"].remove(row)
                    view["comments"].append(row)
                view["pending"] = [item for item in view["pending"] if item["change"] != {"kind": "delete_comment", "comment": request["comment"]}]
                comment(row)["body"] = request["body"]
                if request.get("target"):
                    comment(row)["target"] = request["target"]
            response["comment"] = request["comment"] if response.get("error") or request["body"].strip() else None
        elif request["body"].strip():
            identity = "draft-" + str(view["generation"])
            target = request["target"]
            destination = "standalone" if target["kind"] == "pull_request" else "review"
            if target["kind"] == "reply":
                destination = row_for(target["target"]).get("destination", "review")
            if destination == "review" and not view.get("wip"):
                view["wip"] = {"id": "wip-1", "active": False, "conclusion": None}
            row = {"comment": {"origin": "draft", "comment": {"id": identity, "body": request["body"], "author": view["review"]["reviewer"], "target": target}}, "fetched": False, "draft": True, "done": False, "destination": destination, "conflict": False, "summary": False}
            view["comments"].append(row)
            response["comment"] = identity
    elif op == "reaction":
        row = row_for(request["comment"])
        if not row["fetched"] or row.get("draft"):
            response["error"] = "progress is available only for fetched comments"
        elif (directory / "fail_reaction").exists() or ((directory / "fail_reaction_id").exists() and (directory / "fail_reaction_id").read_text().strip() == request["comment"]):
            response["error"] = "Fixture reaction conflict"
        else:
            row["done"] = request["present"]
            view["pending"] = [{"id": "reaction", "change": {"kind": "set_comment_reaction", "comment": request["comment"], "reaction": "+1", "present": request["present"]}}]
    elif op == "viewed":
        if (directory / "fail_viewed").exists():
            response["error"] = "Fixture viewed conflict"
        else:
            next(file for file in view["files"] if file["path"] == request["path"])["viewed"] = request["viewed"]
    elif op in {"summary", "publish_review"}:
        if not view.get("wip"):
            view["wip"] = {"id": "wip-1", "active": False}
        view["wip"]["conclusion"] = request["conclusion"]
        view["pending"] = [item for item in view["pending"] if item["change"]["kind"] != "set_review_conclusion"]
        if request["conclusion"] is not None:
            view["pending"].append({"id": "review-conclusion", "change": {"kind": "set_review_conclusion", "conclusion": request["conclusion"]}})
    elif op == "move":
        selected = set(request["comments"])
        if not selected:
            response["error"] = "select unsent local inline drafts to move"
        for identity in selected:
            row = row_for(identity)
            if not (row["comment"]["origin"] == "draft" and row.get("destination") and comment(row)["target"]["kind"] in {"diff", "reply"}):
                response["error"] = "only unsent local inline drafts can move into the review"
            elif comment(row)["author"]["id"] != view["review"]["reviewer"]["id"]:
                response["error"] = "editing and deletion require verified ownership"
        for row in view["comments"]:
            target = comment(row)["target"]
            if target["kind"] != "reply":
                continue
            parent = row_for(target["target"])
            if (comment(row)["id"] in selected and target["target"] not in selected and parent.get("destination") not in {None, "review"}) or (target["target"] in selected and comment(row)["id"] not in selected and row.get("destination") not in {None, "review"}):
                response["error"] = "move unpublished parents and replies together"
        if not response.get("error"):
            if not view.get("wip"):
                view["wip"] = {"id": "wip-1", "active": False}
            for identity in selected:
                row_for(identity)["destination"] = "review"
    elif op == "pr_details":
        if view["pr"]["source"]["kind"] != "local_intent":
            response["error"] = "Remote PR title and body are read-only"
        elif request["revision"] != view["pr"]["revision"]:
            response["error"] = "PR metadata changed in another session; reopen before saving"
        else:
            view["pr"].update(title=request["title"], body=request["body"], revision=view["pr"]["revision"] + 1)
    elif op == "submit":
        response["error"] = "use publish to execute the queued review"
    elif op == "delete":
        row = row_for(request["comment"])
        if (directory / "fail_delete").exists() and (directory / "fail_delete").read_text().strip() == request["comment"]:
            response["error"] = "Fixture deletion conflict"
        elif any(comment(child)["target"] == {"kind": "reply", "target": request["comment"]} and child.get("destination") for child in view["comments"]):
            response["error"] = "Remove local replies before deleting their parent"
        elif row["fetched"] or not row.get("destination"):
            response["error"] = "published comments are immutable"
        else:
            view["comments"].remove(row)
            view.setdefault("deleted_comments", []).append(row)
    elif op == "restore":
        row = row_for(request["comment"])
        if not row.get("destination"):
            response["error"] = "published comments are immutable"
        elif row not in view["comments"]:
            view["deleted_comments"].remove(row)
            view["comments"].append(row)
    elif op == "resolve":
        row = row_for(request["comment"])
        conflict = next(item for item in view["conflicts"] if item["comment"] == request["comment"])
        if request["resolution"] == "edit":
            comment(row)["body"] = request["body"]
        elif request["resolution"] == "remote" and conflict["remote"]:
            comment(row)["body"] = conflict["remote"]["body"]
        row["conflict"] = False
        view["conflicts"] = [item for item in view["conflicts"] if item["comment"] != request["comment"]]
    elif op == "publish":
        conclusion = (view.get("wip") or {}).get("conclusion")
        if conclusion and (directory / "fail_submit").exists():
            response["error"] = "Fixture submission failed"
        elif conclusion and conclusion["outcome"] == "comment" and not (conclusion.get("body") or "").strip():
            response["error"] = "Comment requires a nonblank summary"
        else:
            published = directory / "published_pr.json"
            if published.exists():
                view["pr"] = json.loads(published.read_text())
                view["locator"] = view["pr"]["source"]["locator"]
            for row in view["comments"]:
                if row.get("destination"):
                    row.pop("destination")
                    row["draft"] = False
            view["pending"] = []
            if conclusion:
                view["wip"] = None
    elif op == "compare_latest" and (directory / "defer_compare").exists():
        response["refresh_deferred"] = True
    elif op == "compare_latest" and (directory / "latest_comparison.json").exists():
        view["comparison"] = json.loads((directory / "latest_comparison.json").read_text())
        view["review"]["read"]["pull_request"]["revision"] = view["comparison"]
    elif op == "refresh" and (directory / "fail_refresh").exists():
        response["error"] = "Fixture refresh failed\r\nProvider diagnostic\n  Final diagnostic"
    elif op == "apply_refresh":
        prepared = request["prepared_refresh"]
        if prepared.get("error"):
            response["error"] = prepared["error"]
        elif prepared.get("deferred"):
            response["refresh_deferred"] = True
        elif prepared.get("fixture_snapshot"):
            view.update(copy.deepcopy(prepared["fixture_snapshot"]))
        elif request.get("compare_latest"):
            if (directory / "defer_compare").exists():
                response["refresh_deferred"] = True
            elif (directory / "latest_comparison.json").exists():
                view["comparison"] = json.loads((directory / "latest_comparison.json").read_text())
                view["review"]["read"]["pull_request"]["revision"] = view["comparison"]
    if response.get("error") and op in {"start", "leave", "save", "delete", "restore", "viewed", "reaction", "summary", "publish_review", "move", "relocate", "resolve", "pr_details"}:
        view = previous
    elif op not in {"active", "open"}:
        view["generation"] += 1
    if op == "save" and not response.get("error"):
        view["saved_comment"] = response.get("comment")
    view["todo"] = [comment(row)["id"] for row in view["comments"] if row["fetched"] and not row.get("draft") and not row["done"]]
    view["done"] = [comment(row)["id"] for row in view["comments"] if row["fetched"] and not row.get("draft") and row["done"]]
    response["workspace"] = view
    return response

if op in {"entry", "route", "pr"}:
    routes_path = directory / "routes.json"
    routes = json.loads(routes_path.read_text()) if routes_path.exists() else {}
    configured = routes.get("pr:" + request["base"] if op == "pr" else request.get("url", "entry"))
    if configured:
        response = configured
    elif op == "route" and (directory / "switch_workspace").exists():
        target = copy.deepcopy(view)
        target["id"] = "workspace-42"
        target["locator"] = request["url"]
        target["review"]["read"]["pull_request"]["id"] = "PR-42"
        target["pr"].update(id="PR-42", source={"kind": "remote", "locator": request["url"]})
        views[target["id"]] = target
        response = {"route": "batch", "batch": target["id"]}
    else:
        response = {"route": "batch", "batch": view["id"]}
elif op == "select":
    view = views[request["batch"]]
    write_json(state_path, view)
    response["workspace"] = view
    response["fresh"] = bool(request.get("fresh_review") and request["fresh_review"] == view["review"]["id"])
elif op == "sessions":
    browser = json.loads((directory / "sessions.json").read_text())
    if request.get("pr"):
        browser["pr"] = request["pr"]
        browser["groups"] = [group for group in browser["groups"] if group["pr"]["id"] == request["pr"]]
    response["sessions"] = browser
elif op == "new":
    source = next((candidate for candidate in views.values() if candidate["review"]["read"]["pull_request"]["id"] == request.get("pr") or candidate.get("locator") == request.get("url")), view)
    view = copy.deepcopy(source)
    view["id"] = "new-batch-" + str(len(views))
    view["generation"] = 0
    view["pending"], view["publications"], view["wip"] = [], [], None
    empty = {key: 0 for key in ("comments", "edits", "deletions", "reactions", "files", "conclusions", "total")}
    view["stats"] = {"status": "draft", "unfinished": True, "pending": empty, "published": empty, "conflicts": 0}
    view["comments"] = [row for row in view["comments"] if row["fetched"]]
    views[view["id"]] = view
    write_json(state_path, view)
    response["workspace"] = view
elif op == "history":
    response["publication"] = json.loads((directory / "history.json").read_text())
elif op == "list":
    if (directory / "fail_list").exists():
        response["error"] = (directory / "fail_list").read_text().rstrip()
    elif (directory / "pull_requests.json").exists():
        response["pull_requests"] = json.loads((directory / "pull_requests.json").read_text())
    else:
        response["pull_requests"] = [
            {"number": 1, "title": "Fixture review", "url": "https://github.com/acme/repo/pull/1", "author": "alice", "assignees": ["me"]},
            {"number": 42, "title": "Other review\nwith details", "url": "https://github.com/acme/repo/pull/42", "author": "bob", "assignees": ["me"]}
        ]
elif op == "code":
    response["code"] = {"path": request["path"], "revision": view["comparison"], "base_content": "one\ntwo\nthree\n", "head_content": "one\nchanged\nthree\n", "binary": False}
    configured = directory / "code_contexts.json"
    if configured.exists():
        response["code"] = json.loads(configured.read_text()).get(request["path"], response["code"])
    if (directory / "empty_head").exists():
        response["code"]["head_content"] = ""
    if (directory / "empty_base").exists():
        response["code"]["base_content"] = ""
elif op == "historical_code":
    configured = directory / "historical_contexts.json"
    overrides = json.loads(configured.read_text()) if configured.exists() else {}
    if request["comment"] in overrides:
        response = overrides[request["comment"]]
    else:
        root = row_for(request["comment"])
        while comment(root)["target"]["kind"] == "reply":
            root = row_for(comment(root)["target"]["target"])
        target = comment(root)["target"]["target"]
        original = target.get("original", target)
        pair = original["revision"]
        source_side = original.get("range", {}).get("start", {}).get("side", "head")
        source = pair[source_side]
        revision = {"base": source, "head": view["comparison"]["head"]}
        location = copy.deepcopy(original)
        location["revision"] = revision
        if location.get("range"):
            location["range"]["start"]["side"] = "base"
            location["range"]["end"]["side"] = "base"
        source_content = "one\nhistorical " + source_side + "\nthree\n"
        head_content = "one\nchanged\nthree\n"
        original_base = None
        if pair.get("base"):
            base_content = "one\ntwo\nthree\n" if pair == view["comparison"] else "one\nhistorical base\nthree\n"
            original_base = {"commit": pair["base"], "path": original["path"], "content": base_content, "missing": False, "provenance": "recorded"}
            if source_side == "base": source_content = base_content
        if pair == view["comparison"] and source_side == "head": source_content = head_content
        response = {
            "anchor": original,
            "diff_hunk": "@@ -1 +1 @@\n-old\n+new",
            "code": {
                "path": original["path"], "base_path": original["path"], "revision": revision,
                "base_content": source_content, "head_content": head_content,
                "original_base": original_base,
                "binary": False, "historical": True, "provenance": "recorded", "location": location,
            },
        }
elif op == "context":
    response["context"] = {"available": False, "diff_hunk": "@@ -1 +1 @@\n-old\n+new", "message": "Recorded hunk only"}
    if (directory / "context_pair").exists():
        response["context"] = {"available": True, "anchor": {"path": "src/file.rs", "revision": {"base": "old-base", "head": "old-head"}, "range": {"start": {"side": "head", "line": 2}, "end": {"side": "head", "line": 2}}}, "base_content": "old\nsecond\n", "head_content": "new\nchanged\n"}
    configured = directory / "contexts.json"
    if configured.exists():
        response["context"] = json.loads(configured.read_text()).get(request["comment"], response["context"])
elif op == "log":
    log_path = directory / "log.json"
    response["log"] = json.loads(log_path.read_text()) if log_path.exists() else [
        {"generation": 0, "summary": "Opened session", "details": ["Fixture history"], "level": "info",
         "raw": json.dumps({"generation": 0, "action": {"action": "open"}}, indent=2).splitlines()}
    ]
elif op == "get":
    response["workspace"] = view
elif op == "actions":
    applied = 0
    generation = request["generation"]
    for index, action in enumerate(request["actions"]):
        attempt = dict(action)
        attempt["op"] = attempt.pop("action")
        attempt["workspace"], attempt["generation"] = request["workspace"], generation
        with (directory / "action_attempts.jsonl").open("a") as stream:
            stream.write(json.dumps(attempt) + "\n")
        result = mutate(attempt)
        if result.get("error"):
            response["error"], response["failed_index"] = result["error"], index
            break
        applied += 1
        generation = view["generation"]
    response["workspace"], response["applied"] = view, applied
    with (directory / "action_results.jsonl").open("a") as stream:
        stream.write(json.dumps(response) + "\n")
elif op in {"active", "open", "refresh", "apply_refresh", "compare_latest", "publish", "submit"}:
    response = mutate(request)
else:
    response["error"] = "Unknown workspace operation: " + op

if response.get("workspace"):
    views[view["id"]] = view
    if view["id"] == active_id:
        write_json(state_path, view)
write_json(views_path, views)

if publication_applied:
    write_json(directory / "post_published.json", {"tag": publication_case["tag"], "snapshot": publication_case["remote"]})
    publication_event("published", tag=publication_case["tag"])

print(json.dumps(response))
