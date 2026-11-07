# nexo in CI

A composite GitHub Action (`Locotir/nexo@main`) and a GitLab CI template. Three modes: `review` a pull request, answer `@nexo` comments, or run a one-off `prompt`.

## Setup in 3 steps

1. Add your provider key as a repository secret, for example `ANTHROPIC_API_KEY` (Settings, Secrets and variables, Actions).
2. Copy a workflow from [`examples/github/`](../examples/github) into `.github/workflows/`: `nexo-review.yml` (every PR), `nexo-comment.yml` (`@nexo` in a comment), `nexo-nightly.yml` (a prompt on a schedule). Set `provider` (and `model` if you want one).
3. Open a pull request. The first run downloads the newest release build, checks it against the release's `SHA256SUMS`, and caches it per release tag.

GitLab: copy [`examples/gitlab/.gitlab-ci.yml`](../examples/gitlab/.gitlab-ci.yml), add the masked variable `NEXO_API_KEY` and a project access token (scope `api`) as `NEXO_GITLAB_TOKEN`.

## Inputs

| Input | Meaning |
| --- | --- |
| `mode` | `review`, `comment` or `prompt`. |
| `prompt` | The task (prompt mode). |
| `provider`, `model` | As `nexo --provider`, `--model`. |
| `api_key` | The provider key. It reaches nexo as an environment variable (`<PROVIDER>_API_KEY`, or the name in `api_key_env`), never on a command line. |
| `base` | Review: base branch. Default: the PR base. |
| `max_turns` | Stops after N tool rounds (exit 4). Comment mode default 30. |
| `allowed_tools` | `--allowed-tools` list. Comment default: `read,ls,glob,grep,edit,write,todo,bash(git *)`. Add `bash(cargo test*)` to let it run tests. |
| `github_token` | Default `github.token`. |
| `fail_on_high` | Review: fail the job when a high finding exists. |
| `nexo_version` | `latest` or a release tag such as `build-1a2b3c4`. |

Outputs: `result`, `cost`, `exit_code` (prompt; `result` and `exit_code` also in comment mode), `findings`, `high` (review).

## Modes

- **review**: runs `nexo review --base origin/<base> --json` (read-only). Posts one summary comment, edited on every push, and inline comments for findings with a file and line (the old inline set is removed first). If GitHub refuses an inline line because it is outside the diff, the finding stays in the summary.
- **comment**: on `issue_comment` and `pull_request_review_comment` containing `@nexo`, only when the commenter has write, maintain or admin access (checked through the API). Bots are ignored. On a PR it checks out the PR branch, runs `nexo -p` with the comment as the task, commits and pushes. On an issue it pushes `nexo/issue-<n>-<run>` and opens a PR. It replies with the answer, a link and a cost line. Pull requests from forks are refused.
- **prompt**: `nexo -p <prompt> --output-format json`; the step exits with nexo's exit code ([codes](../nexo/docs/HEADLESS.md)). Use `continue-on-error` and `if: always()` to read the outputs of a failed run.

## Permissions block

```yaml
permissions:
  contents: write        # comment mode pushes; review and prompt need only read
  pull-requests: write   # review comments, opening PRs
  issues: write          # summary and reply comments
```

Use `actions/checkout` with `fetch-depth: 0` so the merge base is there. A commit pushed with `GITHUB_TOKEN` does not start other workflows; pass a personal or app token as `github_token` if you need that.

## Security notes

- Review mode runs `nexo review`, which is fixed to read-only permissions: text in the PR (title, body, code comments, file contents) can steer the model but cannot make it write files or run commands. Keep it that way: do not give review jobs `allowed_tools` with `bash` or `write`.
- Do not run these workflows with `pull_request_target` on fork code. The examples use `pull_request` and skip forks, which have no secrets anyway.
- Comment mode obeys only people with write access, but the code it reads may come from anyone. Keep `allowed_tools` narrow, keep `max_turns` low, protect the default branch (the action pushes to PR branches and new `nexo/` branches only), and review what it pushes.
- Issue text is given to the model marked as untrusted data, not as instructions. It is still model input: prefer the `@nexo` comment of a maintainer for the task.
- The key is read from the `api_key` input into the process environment, masked in the log, and removed from the environment of the action script. Anything nexo's own tools run (such as `bash`) can read the variable of the key; keep `bash` patterns tight.
- Pin `uses: Locotir/nexo@<commit>` and `nexo_version: build-<sha7>` for reproducible runs. Downloads are checked against `SHA256SUMS` published with each release.
- Only Linux and Windows runners (x86_64, aarch64) have release builds.
