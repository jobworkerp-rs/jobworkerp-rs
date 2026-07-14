const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');

const repoRoot = path.resolve(__dirname, '../../..');
const pinnedWorkflowTargetPattern = /target: https:\/\/gitea\.sutr\.app\/jobworkerp-rs\/jobworkerp-rs\/raw\/commit\/([0-9a-f]{40})\//;

function readRepoFile(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), 'utf8');
}

function readFileAtCommit(commit, relativePath) {
  return execFileSync('git', ['show', `${commit}:${relativePath}`], {
    cwd: repoRoot,
    encoding: 'utf8',
  });
}

function getPinnedWorkflowCommit(workflow) {
  const targetCommit = workflow.match(pinnedWorkflowTargetPattern)?.[1];

  assert.ok(targetCommit, 'workflow target must contain an immutable commit SHA');
  return targetCommit;
}

test('Gitea PR workflow dispatches an immutable trusted jobworkerp workflow', () => {
  const workflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');

  assert.match(workflow, /pull_request:/);
  assert.match(
    workflow,
    /uses: https:\/\/gitea\.sutr\.app\/jobworkerp-rs\/jobworkerp-actions\/jobworkerp-run@1ecd9a0f9a58fabadb4609871e8e0d349f7ba010/,
  );
  assert.doesNotMatch(workflow, /actions\/checkout@v4/);
  assert.match(
    workflow,
    /target: https:\/\/gitea\.sutr\.app\/jobworkerp-rs\/jobworkerp-rs\/raw\/commit\/fec64bc23a249e8521b1413ee6638f425b597f2e\/\.gitea\/jobworkerp\/workflows\/gitea-pr-auto-review-fix-workflow\.yaml/,
  );
  assert.doesNotMatch(workflow, /raw\/branch\//);
  assert.doesNotMatch(workflow, /pr_head_branch/);
  assert.match(workflow, /"max_iterations": \$\{\{ github\.event\.inputs\.max_iterations \|\| '10' \}\}/);
});

test('pinned jobworkerp workflow revision initializes post-review status variables', () => {
  const actionWorkflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');
  const targetCommit = getPinnedWorkflowCommit(actionWorkflow);

  const targetWorkflow = readFileAtCommit(
    targetCommit,
    '.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml',
  );
  const initialization = targetWorkflow.match(/- initializeVariables:\n([\s\S]*?)\n\s*- mainProcessWithErrorHandling:/)?.[1] ?? '';

  assert.match(initialization, /post_fix_status: ""/);
  assert.match(initialization, /post_refactor_status: ""/);
  assert.match(targetWorkflow, /timeout:\n\s+after:\n\s+hours: 6\n\ndo:/);
});

test('Gitea Action does not load a workflow definition from a PR branch', () => {
  const workflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');

  assert.match(workflow, /id: fetch-pr-data/);
  assert.doesNotMatch(workflow, /pr_head_branch/);
  assert.doesNotMatch(workflow, /raw\/branch\//);
  assert.doesNotMatch(workflow, /target: .*\$\{\{/);
});

test('Gitea Action uses a prebuilt client instead of requiring Cargo on the runner', () => {
  const workflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');

  assert.match(workflow, /client-command: \$\{\{ vars\.JOBWORKERP_CLIENT_COMMAND \|\| 'jobworkerp-client' \}\}/);
  assert.match(workflow, /client-binary-url: \$\{\{ vars\.JOBWORKERP_CLIENT_BINARY_URL \}\}/);
  assert.match(workflow, /client-source-repository: ""/);
});

test('Gitea Action routes only Docker runner tasks to the docker channel', () => {
  const actionWorkflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');
  const jobworkerpWorkflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.doesNotMatch(actionWorkflow, /docker_channel/);
  assert.doesNotMatch(jobworkerpWorkflow, /docker_channel/);
  assert.equal((jobworkerpWorkflow.match(/name: DOCKER/g) ?? []).length, 3);
  assert.equal((jobworkerpWorkflow.match(/channel: "docker"/g) ?? []).length, 3);
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

test('Gitea Action auto-runs review only when a PR is opened or reopened', () => {
  const workflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');

  assert.match(workflow, /pull_request:\n\s+types:\n\s+- opened\n\s+- reopened/);
  assert.doesNotMatch(workflow, /\n\s+- synchronize/);
  assert.match(workflow, /workflow_dispatch:/);
});

test('CI runs for every push without waiting for a separate workflow', () => {
  const workflow = readRepoFile('.gitea/workflows/ci.yaml');

  assert.match(workflow, /on:\n\s+push:/);
  assert.doesNotMatch(workflow, /pull_request:/);
  assert.doesNotMatch(workflow, /workflow_run:/);
  assert.doesNotMatch(workflow, /github\.event\.workflow_run|github\.event\.pull_request\.head\.ref/);
});

test('Gitea Action verifies the nested jobworkerp workflow result', () => {
  const workflow = readRepoFile('.gitea/workflows/pr-auto-review-fix.yaml');
  const successfulResult = JSON.stringify({
    id: 'job-1',
    output: JSON.stringify({ status: 'success', pushed: true }),
  });
  const failedResult = JSON.stringify({
    id: 'job-2',
    output: JSON.stringify({ status: 'failed', error_message: 'agent failed' }),
  });
  const faultedResult = JSON.stringify({
    id: 'error',
    status: 'Faulted',
    errorMessage: "Task 'ROOT' timed out after 21600s",
  });
  const workflowStatus = (result) => {
    const parsed = JSON.parse(result);

    if (typeof parsed.output !== 'string') {
      throw new Error(parsed.errorMessage ?? 'jobworkerp action returned no workflow result');
    }
    return JSON.parse(parsed.output).status;
  };

  assert.match(workflow, /JOBWORKERP_RESULT: \$\{\{ steps\.pr-review-fix\.outputs\.result \}\}/);
  assert.match(workflow, /if \(\.output \| type\) == "string" then/);
  assert.match(workflow, /error\(\.errorMessage \/\/ "jobworkerp action returned no workflow result"\)/);
  assert.equal(workflowStatus(successfulResult), 'success');
  assert.equal(workflowStatus(failedResult), 'failed');
  assert.throws(() => workflowStatus(faultedResult), /Task 'ROOT' timed out after 21600s/);
});

test('jobworkerp workflow has six-hour root and main process timeouts', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.match(workflow, /timeout:\n\s+after:\n\s+hours: 6\n\ndo:/);
  const mainProcess = workflow.match(/- mainProcessWithErrorHandling:\n([\s\S]*?)\noutput:/)?.[1] ?? '';

  assert.match(mainProcess, /catch:[\s\S]*?\n\s+timeout:\n\s+after:\n\s+hours: 6/);
});

test('prompt files are written in bounded base64 chunks', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  for (const prompt of ['Review', 'Fix', 'Refactor']) {
    assert.match(workflow, new RegExp(`encode${prompt}Prompt:`));
    assert.match(workflow, new RegExp(`split${prompt}Prompt:`));
    assert.match(workflow, new RegExp(`write${prompt}PromptChunks:`));
  }
  assert.equal((workflow.match(/range\(0; length; 32768\)/g) ?? []).length, 3);
  assert.equal((workflow.match(/base64 -d >>/g) ?? []).length, 3);
  assert.doesNotMatch(workflow, /echo '" \+ \(\$\w+_prompt \| encode_base64\) \+ "' \| base64 -d/);
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
  assert.doesNotMatch(actionWorkflow, /JOBWORKERP_CLONE_BASE_PATH|clone_base_path/);
  assert.match(jobworkerpWorkflow, /\$_secrets\.token/);
  assert.match(jobworkerpWorkflow, /http\.extraHeader=Authorization: Basic/);
  assert.match(jobworkerpWorkflow, /\\"clone\\", \\"--no-checkout\\", \$effective_base_clone_url, \$effective_worktree_path/);
  assert.match(jobworkerpWorkflow, /\\"-C\\", \$effective_worktree_path, \\"-c\\", \\"http\.extraHeader=Authorization: Basic/);
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

test('review agent allows a three-hour execution window', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');
  const reviewAgent = workflow.match(/- runReviewAgent:\n([\s\S]*?)\n\s*- cleanupReviewPromptDir:/)?.[1] ?? '';

  assert.match(reviewAgent, /timeout_sec: 10800/);
  assert.match(reviewAgent, /timeout:\n\s+after:\n\s+minutes: 180/);
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
  const buildCloneUrl = workflow.match(/- buildHeadCloneUrl:\n([\s\S]*?)\n\s*- prepareWorktreeParentDir:/)?.[1] ?? '';

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
  assert.match(parseJudge, /\$judge_raw \| fromjson/);
  assert.match(parseJudge, /catch null/);
  assert.doesNotMatch(parseJudge, /match\(/);
  assert.doesNotMatch(parseJudge, /needs_fix:|fix_prompt:/);
  assert.match(extractDecision, /needs_fix: "\$\{if \$judge_json\.needs_fix == false then false else true end\}"/);
  assert.match(extractDecision, /fix_prompt:/);
});

test('each execution uses and removes its own independent clone', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.match(
    workflow,
    /effective_worktree_parent_path:[\s\S]*?worktree_base_path[\s\S]*?"\/pr-auto-review-fix\//,
  );
  assert.match(workflow, /- createExecutionWorktreePath:/);
  assert.match(workflow, /command: "mktemp"/);
  assert.match(workflow, /createExecutionWorktreePath:[\s\S]*?effective_worktree_parent_path[\s\S]*?pull_number[\s\S]*?XXXXXX[\s\S]*?effective_worktree_root/);
  assert.match(workflow, /- setExecutionWorktreePath:[\s\S]*?effective_worktree_root[\s\S]*?repository/);
  assert.doesNotMatch(workflow, /- checkBaseClone:/);
  assert.doesNotMatch(workflow, /- cloneIfNeeded:/);
  assert.match(workflow, /cloneForExecution:[\s\S]*?\\"--no-checkout\\"[\s\S]*?\$effective_worktree_path/);
  assert.doesNotMatch(workflow, /--bare/);
  assert.doesNotMatch(workflow, /- createWorktree:/);
  assert.match(workflow, /- cleanupWorktree:\n[\s\S]*?command: "rm"[\s\S]*?\$effective_worktree_root/);
  assert.match(workflow, /- cleanupWorktreeOnError:\n[\s\S]*?command: "rm"[\s\S]*?\$effective_worktree_root/);
});

test('agent prompt directories are created in the shared worktree', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.match(workflow, /\$effective_worktree_path \+ \\"\/\.jobworkerp-prompt-XXXXXX\\"/);
  assert.doesNotMatch(workflow, /\/tmp\/cwapp-pr-(review|fix|refactor)-/);
});

test('cleanup variables are initialized before error handlers are parsed', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');
  const initialization = workflow.match(/- initializeVariables:\n([\s\S]*?)\n\s*- mainProcessWithErrorHandling:/)?.[1] ?? '';

  assert.match(initialization, /effective_worktree_path: ""/);
  assert.match(initialization, /effective_worktree_root: ""/);
  assert.match(initialization, /post_fix_status: ""/);
  assert.match(initialization, /post_refactor_status: ""/);
});

test('workflow keeps the named try task form used by the workflow executor', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');

  assert.match(workflow, /\n  - mainProcessWithErrorHandling:\n      try:\n        - extractPRFields:/);
  assert.match(workflow, /\n      catch:\n        as: caught_error/);
});

test('error cleanup passes command arguments to the COMMAND runner', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');
  const cleanupWorktreeOnError = workflow.match(/- cleanupWorktreeOnError:\n([\s\S]*?)\n\s*- setErrorState:/)?.[1] ?? '';

  assert.match(cleanupWorktreeOnError, /runner:\n\s+name: COMMAND\n\s+arguments:/);
  assert.match(cleanupWorktreeOnError, /args: "\$\{\[\\"-rf\\", \$effective_worktree_root\]\}"/);
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
  assert.match(workflow, /checkout\\", \\"--detach\\", \\"refs\/remotes\/pr-head\//);
});

test('workflow rebases agent commits onto the latest PR head before pushing', () => {
  const workflow = readRepoFile('.gitea/jobworkerp/workflows/gitea-pr-auto-review-fix-workflow.yaml');
  const syncBeforePush = workflow.match(/- synchronizePRHeadBeforePush:\n([\s\S]*?)\n\s*- checkPushNeeded:/)?.[1] ?? '';

  assert.ok(syncBeforePush.includes('\\"fetch\\", $effective_head_clone_url'));
  assert.ok(syncBeforePush.includes('\\"rebase\\", \\"refs/remotes/pr-head/\\" + $pr_head_branch'));
  assert.ok(
    workflow.indexOf('- synchronizePRHeadBeforePush:') < workflow.indexOf('- checkPushNeeded:'),
    'the PR head must be synchronized before calculating commits to push',
  );
  assert.doesNotMatch(syncBeforePush, /--force|--force-with-lease/);
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
