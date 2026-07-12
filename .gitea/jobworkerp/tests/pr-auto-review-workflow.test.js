const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');

const repoRoot = path.resolve(__dirname, '../../..');

function readRepoFile(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), 'utf8');
}

test('Gitea PR workflow dispatches the tracked jobworkerp auto review workflow', () => {
  const workflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');

  assert.match(workflow, /pull_request:/);
  assert.match(
    workflow,
    /uses: https:\/\/gitea\.sutr\.app\/jobworkerp-rs\/jobworkerp-actions\/jobworkerp-run@main/,
  );
  assert.doesNotMatch(workflow, /actions\/checkout@v4/);
  assert.doesNotMatch(workflow, /target: \.gitea\/jobworkerp\/workflows\//);
  assert.match(
    workflow,
    /target: https:\/\/gitea\.sutr\.app\/\$\{\{ github\.repository \}\}\/raw\/branch\/\$\{\{ steps\.fetch-pr-data\.outputs\.pr_head_branch \}\}\/\.gitea\/jobworkerp\/workflows\/gitea-pr-auto-review-fix-workflow\.yaml/,
  );
  assert.match(workflow, /"max_iterations": \$\{\{ github\.event\.inputs\.max_iterations \|\| '10' \}\}/);
});

test('Gitea Action loads the latest workflow from a validated same-repository PR branch', () => {
  const workflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');

  assert.match(workflow, /id: fetch-pr-data/);
  assert.match(workflow, /pr_head_branch="\$\(printf '%s' "\$pr_data" \| jq -er '\.head\.ref'\)"/);
  assert.match(workflow, /echo "pr_head_branch=\$pr_head_branch" >> "\$GITHUB_OUTPUT"/);
  assert.match(workflow, /name: Reject fork PR before loading workflow/);
  assert.match(workflow, /jq -er '\.head\.repo\.full_name'/);
  assert.match(workflow, /\[ "\$head_repository" = "\$GITEA_REPOSITORY" \]/);
});

test('Gitea Action uses a prebuilt client instead of requiring Cargo on the runner', () => {
  const workflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');

  assert.match(workflow, /client-command: \$\{\{ vars\.JOBWORKERP_CLIENT_COMMAND \|\| 'jobworkerp-client' \}\}/);
  assert.match(workflow, /client-binary-url: \$\{\{ vars\.JOBWORKERP_CLIENT_BINARY_URL \}\}/);
  assert.match(workflow, /client-source-repository: ""/);
});

test('Gitea Action routes only Docker runner tasks to the configured Docker channel', () => {
  const actionWorkflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');
  const jobworkerpWorkflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.match(actionWorkflow, /"docker_channel": "\$\{\{ vars\.JOBWORKERP_DOCKER_CHANNEL \|\| 'docker' \}\}"/);
  assert.match(jobworkerpWorkflow, /docker_channel:/);
  assert.match(jobworkerpWorkflow, /docker_channel: "\$\{\$workflow\.input\.docker_channel \/\/ \\"docker\\"\}"/);
  assert.equal((jobworkerpWorkflow.match(/name: DOCKER/g) ?? []).length, 3);
  assert.equal((jobworkerpWorkflow.match(/channel: "\$\{\$docker_channel\}"/g) ?? []).length, 3);
});

test('Gitea Action ensures protoc is available before invoking the client', () => {
  const workflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');

  assert.match(workflow, /name: Install protoc/);
  assert.match(workflow, /command -v protoc/);
  assert.match(workflow, /apt-get install -y protobuf-compiler/);
});

test('Gitea Action fetches PR data and passes it without an MCP runner', () => {
  const actionWorkflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');
  const jobworkerpWorkflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.match(actionWorkflow, /id: fetch-pr-data/);
  assert.match(actionWorkflow, /Authorization: token \$GITEA_TOKEN/);
  assert.match(actionWorkflow, /pulls\/\$PR_NUMBER/);
  assert.match(actionWorkflow, /"pr_data": \$\{\{ steps\.fetch-pr-data\.outputs\.pr_data \}\}/);
  assert.doesNotMatch(actionWorkflow, /mcp_server_name/);
  assert.match(jobworkerpWorkflow, /pr_data:/);
  assert.doesNotMatch(jobworkerpWorkflow, /- fetchPR:/);
  assert.doesNotMatch(jobworkerpWorkflow, /\$mcp_server_name/);
});

test('Gitea Action runs privileged agents only for same-repository PRs', () => {
  const workflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');

  assert.match(
    workflow,
    /if: github\.event_name == 'workflow_dispatch' \|\| github\.event\.pull_request\.head\.repo\.full_name == github\.repository/,
  );
});

test('Gitea Action fails when the jobworkerp workflow reports failure', () => {
  const workflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');

  assert.match(workflow, /JOBWORKERP_RESULT: \$\{\{ steps\.pr-review-fix\.outputs\.result \}\}/);
  assert.match(workflow, /jq -er '\.status == "success"'/);
});

test('workflow rejects fork PRs before cloning, including manual dispatches', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');
  const extractFields = workflow.match(/- extractPRFields:\n([\s\S]*?)\n\s*- rejectUntrustedHeadRepository:/)?.[1] ?? '';
  const rejection = workflow.match(/- rejectUntrustedHeadRepository:\n([\s\S]*?)\n\s*- buildHeadCloneUrl:/)?.[1] ?? '';

  assert.match(extractFields, /pr_head_repository_full_name:/);
  assert.match(rejection, /\$pr_head_repository_full_name != \(\$workflow\.input\.owner \+ \\"\/\\" \+ \$workflow\.input\.repo\)/);
  assert.match(rejection, /command: "sh"/);
  assert.match(rejection, /args: \["-c", "exit 1"\]/);
});

test('Gitea Action passes the write token through the protected workflow context', () => {
  const actionWorkflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');
  const jobworkerpWorkflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.match(actionWorkflow, /GITEA_TOKEN: \$\{\{ secrets\.GITEA_TOKEN \}\}/);
  assert.match(actionWorkflow, /action-context: token/);
  assert.match(actionWorkflow, /action-token-env: GITEA_TOKEN/);
  assert.match(jobworkerpWorkflow, /\$_secrets\.token/);
  assert.match(jobworkerpWorkflow, /http\.extraHeader=Authorization: Basic/);
  assert.match(jobworkerpWorkflow, /\\"clone\\", \\"--bare\\", \$effective_base_clone_url/);
  assert.match(jobworkerpWorkflow, /\\"-C\\", \$effective_clone_path, \\"-c\\", \\"http\.extraHeader=Authorization: Basic/);
  assert.match(jobworkerpWorkflow, /\\"push\\", \$effective_head_clone_url/);
  assert.doesNotMatch(jobworkerpWorkflow, /authenticated_(base|head)_clone_url/);
  assert.doesNotMatch(jobworkerpWorkflow, /agent_.*token|token.*agent_/i);
});

test('jobworkerp workflow contains the bounded review/fix/refactor flow', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.match(workflow, /name: "gitea-pr-auto-review-fix-workflow"/);
  assert.match(workflow, /effective_max_iterations:/);
  assert.match(workflow, /reviewFixLoop:/);
  assert.match(workflow, /while: "\$\{\$continue_review == true\}"/);
  assert.match(workflow, /runnerName: LLM/);
  assert.match(workflow, /runFixAgent:/);
  assert.match(workflow, /runRefactorAgent:/);
  assert.match(workflow, /pushChanges:/);
});

test('review judge prompt expands the actual review output', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');
  const judgePrompt = workflow.match(/text: \|\n([\s\S]*?)\n\s*system_prompt:/)?.[1] ?? '';

  assert.match(judgePrompt, /\$\$\{/);
  assert.match(judgePrompt, /{{ review_output }}/);
});

test('fallback commits stage untracked files as well as tracked updates', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.doesNotMatch(workflow, /git add -u && git diff --cached --quiet/);
  assert.match(workflow, /git ls-files --modified --deleted --others --exclude-standard -z/);
});

test('jobworkerp workflow has no unresolved deployment placeholders', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.doesNotMatch(workflow, /__[A-Z0-9_]+__/);
  assert.match(workflow, /agent_config_volumes: "\$\{\$workflow\.input\.agent_config_volumes \/\/ \[\]\}"/);
});

test('jobworkerp workflow uses PR head repository for branch operations', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.match(workflow, /pr_head_owner:/);
  assert.match(workflow, /pr_head_repo:/);
  assert.match(workflow, /effective_head_clone_url:/);
  assert.match(workflow, /fetchHeadLatest:/);
  assert.match(workflow, /refs\/remotes\/pr-head\//);
  assert.match(workflow, /fetchBaseLatest:[\s\S]*?\$pr_base_branch[\s\S]*?refs\/remotes\/origin/);
  assert.doesNotMatch(workflow, /\+refs\/heads\/\*:refs\/remotes\/origin\/\*/);
  assert.match(workflow, /\\"push\\", \$effective_head_clone_url/);
});

test('dependent PR clone fields are evaluated in separate set tasks', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');
  const extractFields = workflow.match(/- extractPRFields:\n([\s\S]*?)\n\s*- buildHeadCloneUrl:/)?.[1] ?? '';
  const buildCloneUrl = workflow.match(/- buildHeadCloneUrl:\n([\s\S]*?)\n\s*- prepareCloneParentDir:/)?.[1] ?? '';

  assert.doesNotMatch(extractFields, /effective_head_clone_url:/);
  assert.match(buildCloneUrl, /effective_head_clone_url:/);
  assert.match(buildCloneUrl, /join\(\$pr_head_owner\)/);
  assert.match(buildCloneUrl, /join\(\$pr_head_repo\)/);
});

test('review judge fields are evaluated in separate set tasks', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');
  const parseJudge = workflow.match(/- parseReviewJudge:\n([\s\S]*?)\n\s*- extractReviewJudgeDecision:/)?.[1] ?? '';
  const extractDecision = workflow.match(/- extractReviewJudgeDecision:\n([\s\S]*?)\n\s*- routeAfterJudge:/)?.[1] ?? '';

  assert.match(parseJudge, /judge_json:/);
  assert.doesNotMatch(parseJudge, /needs_fix:|fix_prompt:/);
  assert.match(extractDecision, /needs_fix: "\$\{\$judge_json\.needs_fix \/\/ true\}"/);
  assert.match(extractDecision, /fix_prompt:/);
});

test('each execution uses and removes its own bare clone', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.match(workflow, /- createExecutionClonePath:/);
  assert.match(workflow, /command: "mktemp"/);
  assert.match(workflow, /createExecutionClonePath:[\s\S]*?effective_clone_parent_path[\s\S]*?pull_number[\s\S]*?XXXXXX/);
  assert.doesNotMatch(workflow, /- checkBaseClone:/);
  assert.doesNotMatch(workflow, /- cloneIfNeeded:/);
  assert.match(workflow, /- cleanupClone:\n[\s\S]*?command: "rm"[\s\S]*?\$effective_clone_path/);
  assert.match(workflow, /- cleanupCloneOnError:\n[\s\S]*?command: "rm"[\s\S]*?\$effective_clone_path/);
});

test('agent prompt directories are created in the shared worktree', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.match(workflow, /\$effective_worktree_path \+ \\"\/\.jobworkerp-prompt-XXXXXX\\"/);
  assert.doesNotMatch(workflow, /\/tmp\/cwapp-pr-(review|fix|refactor)-/);
});

test('cleanup variables are initialized before error handlers are parsed', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');
  const initialization = workflow.match(/- initializeVariables:\n([\s\S]*?)\n\s*- mainProcessWithErrorHandling:/)?.[1] ?? '';

  assert.match(initialization, /effective_clone_path: ""/);
});

test('workflow keeps the named try task form used by the workflow executor', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.match(workflow, /\n  - mainProcessWithErrorHandling:\n      try:\n        - extractPRFields:/);
  assert.match(workflow, /\n      catch:\n        as: caught_error/);
});

test('error cleanup passes command arguments to the COMMAND runner', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');
  const cleanupCloneOnError = workflow.match(/- cleanupCloneOnError:\n([\s\S]*?)\n\s*- setErrorState:/)?.[1] ?? '';

  assert.match(cleanupCloneOnError, /runner:\n\s+name: COMMAND\n\s+arguments:/);
  assert.match(cleanupCloneOnError, /args: "\$\{\[\\"-rf\\", \$effective_clone_path\]\}"/);
});

test('workflow error output preserves the caught error detail', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');
  const setErrorState = workflow.match(/- setErrorState:\n([\s\S]*?)\n\noutput:/)?.[1] ?? '';

  assert.match(setErrorState, /error_message: "\$\{\$caught_error\.detail/);
  assert.doesNotMatch(setErrorState, /\$caught_error\.message/);
});

test('LLM judge chat message uses protobuf MessageContent JSON shape', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.doesNotMatch(workflow, /- role: user\n\s+content: \|/);
  assert.match(workflow, /- role: user\n\s+content:\n\s+text: \|/);
});

test('LLM judge result extracts nested content text first', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.match(workflow, /judge_raw: "\$\{\.content\.text \/\/ \.text/);
});

test('worktree checkout does not reuse PR head branch as a local branch', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.doesNotMatch(workflow, /\\"-B\\", \$pr_head_branch/);
  assert.match(workflow, /\\"--detach\\", \$effective_worktree_path, \\"refs\/remotes\/pr-head\/\\" \+ \$pr_head_branch/);
});

test('LLM settings come from workflow input without undefined context variables', () => {
  const actionWorkflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');
  const jobworkerpWorkflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.doesNotMatch(jobworkerpWorkflow, /\$ollamaBaseUrl|\$ollamaModel/);
  assert.match(jobworkerpWorkflow, /llm_base_url:/);
  assert.match(jobworkerpWorkflow, /llm_model:/);
  assert.match(jobworkerpWorkflow, /llm_base_url: "\$\{\$workflow\.input\.llm_base_url \/\/ \\"http:\/\/localhost:11434\\"\}"/);
  assert.match(jobworkerpWorkflow, /llm_model: "\$\{\$workflow\.input\.llm_model \/\/ \\"qwen3\.6:27b\\"\}"/);
  assert.match(actionWorkflow, /"llm_base_url": "\$\{\{ vars\.LLM_BASE_URL \|\| 'http:\/\/localhost:11434' \}\}"/);
  assert.match(actionWorkflow, /"llm_model": "\$\{\{ vars\.LLM_MODEL \|\| 'qwen3\.6:27b' \}\}"/);
});
