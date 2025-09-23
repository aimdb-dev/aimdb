---
mode: agent
description: "Generate a changelog in Markdown format summarizing changes between the current branch and a specified target branch"
---

# Task: Propose a Changelog

Your task is to generate a changelog in Markdown format. The changelog should summarize the changes between the current working branch and a user-specified target branch.

## Requirements

1.  **Identify Branches**:
    *   The **current branch** is the branch you are currently on.
    *   You must ask the user to specify the **target branch** (e.g., `main`, `develop`) to compare against.

2.  **Gather Information**:
    *   Use `git log <target_branch>..HEAD --oneline` to get a summary of commits.
    *   Use `git diff <target_branch>..HEAD` to analyze the detailed code changes.
    *   Use `gh pr view --json number,title,body,commits,linkedIssues` to get the context of the pull request for the current branch. This will provide any linked issues. If no PR is associated with the branch or no issues are linked, proceed without them.

3.  **Analyze and Categorize**:
    *   Read through the commit messages and diffs to understand the purpose of each change.
    *   Categorize each change into one of the following sections:
        *   `### Added` (for new features)
        *   `### Changed` (for changes in existing functionality)
        *   `### Fixed` (for bug fixes)
        *   `### Removed` (for now-removed features)
        *   `### Security` (in case of vulnerabilities)

4.  **Draft the Changelog**:
    *   For each categorized change, write a user-friendly description. The description should be comprehensive enough to explain the change but not overwhelming with technical jargon. Focus on the "what" and "why" from a user's perspective.
    *   If a change is linked to a GitHub issue or PR, include a reference to it (e.g., `[#123]`).

## Output Format

The final output must be a Markdown document with the following structure. Do not include empty sections.

```markdown
# Changelog

## [Unreleased]

### Added
- New feature A that does something cool. ([#123])
- New command `make check` for development workflow. ([#125])

### Fixed
- Resolved a panic when processing empty data streams. ([#124])

### Changed
- Updated the core engine to improve performance by 20%. ([#126])
```

## Success Criteria

*   The agent correctly identifies the current and target branches.
*   The agent uses `git` and `gh` commands to gather all necessary information.
*   The final changelog is well-structured, accurate, and follows the specified Markdown format.
*   All significant changes are included and correctly categorized.