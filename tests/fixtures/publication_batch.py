"""Disposable GitHub GraphQL responses for publication integration tests."""
import json
import os
import pathlib
import re
import sys

root = pathlib.Path(os.environ["PROVIDER_ROOT"])
case = os.environ["PROVIDER_CASE"]
request = None if sys.argv[1] == "reviews" else json.load(sys.stdin)


def read(name, default):
    path = root / name
    return json.loads(path.read_text()) if path.exists() else default


def write(name, value):
    (root / name).write_text(json.dumps(value))


def page(nodes):
    return dict(totalCount=len(nodes), nodes=nodes,
                pageInfo=dict(hasNextPage=False, endCursor=None))


def actor():
    return dict(databaseId=7, login="reviewer")


def comment(identity, body):
    return dict(id="C" + str(identity), fullDatabaseId=str(identity), body=body,
                author=actor(), createdAt=f"2026-01-01T00:00:{identity % 60:02d}Z",
                reactions=page([]))


if sys.argv[1] == "reviews":
    pending = read("pending-review.json", None)
    if case == "comment_resource_submitted" and (root / "resource-failed").exists():
        (root / "submitted-during-read").touch()
    if case == "pending_reuse":
        pending = dict(id=900, node_id="R900", commit_id="old-head", state="PENDING", user=dict(id=7, login="reviewer"))
    print(json.dumps([[pending] if pending else []]))
    sys.exit(0)


if sys.argv[1] == "files":
    files = read("snapshot-seed.json", {}).get("read", {}).get("files", [dict(path="file.rs")])
    print(json.dumps([[dict(filename=item["path"], status="modified", additions=12, deletions=3) for item in files]]))
    sys.exit(0)


if sys.argv[1] == "batch":
    data, errors = {}, []
    fields = dict(re.findall(r"(op\d+)\s*:\s*(\w+)\s*\(", request["query"]))
    for alias, field in fields.items():
        value = request["variables"]["input" + alias[2:]]
        resource_failure = case.startswith("comment_resource") and not (root / "resource-failed").exists()
        if resource_failure and alias == "op7":
            (root / "resource-failed").touch()
            data[alias] = None
            errors.append(dict(type="RESOURCE_LIMITS_EXCEEDED", path=[alias],
                               message="Resource limits for this query exceeded."))
            continue
        failure = None
        rejected = root / "rejected"
        progress = root / "progress"
        if field in ("addPullRequestReviewThread", "addComment"):
            if case == "comment_partial" and alias == "op1" and not rejected.exists():
                rejected.touch()
                failure = "temporarily rejected comment"
            if case == "anchor_rejection" or (case == "schema_rejection" and not rejected.exists()):
                rejected.touch()
                failure = os.environ["PROVIDER_REJECTION_ERROR"]
            if case == "unknown" and field == "addComment":
                sys.stderr.write("connection lost after sending")
                sys.exit(1)
            if case == "uncertain_review" and field == "addPullRequestReviewThread":
                sys.stderr.write("connection lost after preparing comment")
                sys.exit(1)
        if field in ("markFileAsViewed", "unmarkFileAsViewed"):
            if case == "viewed_cache_failure" and alias == "op1" and not progress.exists():
                progress.touch()
                failure = "HTTP 422 viewed state rejected"
            if case in ("progress", "bulk_boundary_failure") and not progress.exists():
                progress.touch()
                failure = "HTTP 422 viewed state rejected"
            if case == "uncertain_progress" and not progress.exists():
                progress.touch()
                sys.stderr.write("connection lost after setting viewed state")
                sys.exit(1)
        if failure:
            data[alias] = None
            errors.append(dict(message=failure, path=[alias], type="UNPROCESSABLE"))
            continue
        payload = dict(clientMutationId=value["clientMutationId"])
        if field in ("addPullRequestReviewThread", "addPullRequestReviewThreadReply", "addComment"):
            comments = read("batch-comments.json", [])
            identity = 201 + len(comments)
            if field == "addComment":
                identity = 300 + sum(item["kind"] == "issue" for item in comments)
                (root / "issue").touch()
            node = dict(id="C" + str(identity), databaseId=identity)
            if field == "addComment":
                payload["commentEdge"] = dict(node=node)
                kind = "issue"
            elif field == "addPullRequestReviewThreadReply":
                payload["comment"] = node
                kind = "reply"
            else:
                payload["thread"] = dict(id="T" + str(identity), comments=dict(nodes=[node]))
                kind = "root"
            comments.append(dict(value, kind=kind, identity=identity))
            write("batch-comments.json", comments)
        elif field in ("addReaction", "removeReaction"):
            reactions = read("batch-reactions.json", [])
            reactions.append(dict(value, present=field == "addReaction"))
            write("batch-reactions.json", reactions)
            if case == "alias_reaction" and not (root / "alias").exists():
                (root / "alias").touch()
                sys.stderr.write("connection lost reaction response")
                sys.exit(1)
        elif field in ("markFileAsViewed", "unmarkFileAsViewed"):
            viewed = read("batch-viewed.json", {})
            viewed[value["path"]] = field == "markFileAsViewed"
            write("batch-viewed.json", viewed)
        if case == "comment_unknown" and alias == "op1":
            payload.pop("thread", None)
        if resource_failure and alias == "op6":
            payload["thread"]["comments"] = None
            if case == "comment_resource_node":
                payload["thread"]["comments"] = dict(nodes=[dict(id="C999999")])
            if case == "comment_resource_database":
                payload["thread"]["comments"] = dict(nodes=[dict(databaseId=999999)])
        if case == "comment_internal" and alias == "op1":
            # The resolver failed after creating the comment. A root-level
            # error path is not proof that the mutation had no side effect.
            data[alias] = None
            errors.append(dict(type="INTERNAL", path=[alias], message="Resolver failed after write"))
            continue
        data[alias] = payload
    response = dict(data=data)
    if errors:
        response["errors"] = errors
    print(json.dumps(response))
    # gh may return useful partial GraphQL data with a nonzero exit status.
    sys.exit(1 if errors else 0)

if case == "reply_lookup_fail" and not (root / "thread-lookup").exists():
    (root / "thread-lookup").touch()
    sys.stderr.write("HTTP 503 thread lookup unavailable")
    sys.exit(1)

seed = read("snapshot-seed.json", {})
read_state = seed.get("read", {})
identities = {(item["resource"], item["core_id"]): item["remote_id"]
              for item in seed.get("mappings", [])}
issues, threads, reviews = [], [], []
for item in read_state.get("comments", []):
    target, core = item["target"], item["id"]
    resource = "issue_comment" if target["kind"] == "pull_request" else "review_comment"
    if (resource, core) not in identities:
        continue
    identity = int(identities[(resource, core)])
    node = comment(identity, item["body"])
    if resource == "issue_comment":
        if case == "alias_edit" and (root / "alias").exists():
            node["body"] = "Local"
        if case == "alias_reaction" and (root / "alias").exists():
            node["reactions"] = page([dict(id="R501", content="THUMBS_UP", user=actor())])
        issues.append(node)
    elif target["kind"] == "diff":
        source = target["target"]
        anchor = source.get("current") or source
        revision = anchor["revision"]
        head = revision.get("head", "old-head")
        span = anchor.get("range") or {}
        start = span.get("start", dict(line=2, side="head"))
        end = span.get("end", dict(line=4, side="head"))
        node.update(path=anchor["path"], line=end["line"], originalLine=end["line"],
                    startLine=start["line"], originalStartLine=start["line"],
                    commit=dict(oid=head), originalCommit=dict(oid=head),
                    replyTo=None, pullRequestReview=dict(fullDatabaseId="100"),
                    subjectType="LINE", diffHunk="@@ -2 +2 @@", outdated=False, state="SUBMITTED")
        threads.append(dict(id="T" + str(identity), diffSide="RIGHT", startDiffSide="RIGHT", comments=page([node])))
for value in read("batch-comments.json", []):
    identity = value["identity"]
    node = comment(identity, value["body"])
    if value["kind"] == "issue":
        issues.append(node)
        continue
    parent = value.get("pullRequestReviewThreadId")
    thread = next((item for item in threads if item["id"] == parent), None) if parent else None
    line = value.get("line")
    node.update(path=value.get("path", "file.rs"), line=line, originalLine=line,
                startLine=value.get("startLine"), originalStartLine=value.get("startLine"),
                commit=dict(oid="old-head"), originalCommit=dict(oid="old-head"),
                pullRequestReview=dict(fullDatabaseId="100"), subjectType=value.get("subjectType", "LINE"),
                replyTo=dict(fullDatabaseId=str(thread["comments"]["nodes"][0]["fullDatabaseId"])) if thread else None,
                diffHunk="@@ -2 +2 @@", outdated=False,
                state="SUBMITTED" if (root / "submitted-during-read").exists() else "PENDING")
    if thread:
        thread["comments"]["nodes"].append(node)
        thread["comments"]["totalCount"] += 1
    else:
        threads.append(dict(id="T" + str(identity), diffSide=value.get("side", "RIGHT"),
                            startDiffSide=value.get("startSide"), comments=page([node])))
for item in read_state.get("reviews", []):
    identity = identities.get(("review", item.get("id")))
    if identity:
        reviews.append(dict(id="R" + identity, fullDatabaseId=identity, body="Summary",
                            author=actor(), state="COMMENTED", submittedAt="2026-01-01T00:00:00Z", reactions=page([])))
if case.startswith("comment_resource"):
    pending = read("pending-review.json", None)
    if pending:
        reviews.append(dict(id=pending["node_id"], fullDatabaseId=str(pending["id"]), body="",
                            author=dict(databaseId=pending["user"]["id"], login=pending["user"]["login"]),
                            state="COMMENTED" if (root / "submitted-during-read").exists() else "PENDING",
                            submittedAt=None, reactions=page([])))
viewed = read("batch-viewed.json", {})
files = [dict(path=item["path"], changeType="MODIFIED", viewerViewedState="VIEWED" if viewed.get(item["path"], not case.startswith("comment_resource")) else "UNVIEWED")
         for item in read_state.get("files", [dict(path="file.rs")])]
head = (root / "remote-head").read_text().strip() if case == "uncertain_stale_viewed" else "old-head"
if case == "uncertain_stale_viewed":
    for item in files:
        item["viewerViewedState"] = "UNVIEWED"
pull = dict(id="PR1", fullDatabaseId="1", title="PR", body="", author=None, state="OPEN",
            baseRefOid="old-base", headRefOid=head, headRefName="feature", headRepository=dict(url="https://github.com/acme/repo"),
            changedFiles=len(files), files=page(files), comments=page(issues), reviews=page(reviews), reviewThreads=page(threads))
print(json.dumps(dict(data=dict(viewer=actor(), repository=dict(pullRequest=pull)))))
