//! Quick prompts — pre-defined commands sent to agents with a single keystroke.

/// A quick prompt entry: a key binding, a short label, and the full prompt text.
pub struct QuickPrompt {
    pub key: char,
    pub label: &'static str,
    pub prompt: &'static str,
}

pub const QUICK_PROMPTS: &[QuickPrompt] = &[
    QuickPrompt {
        key: '1',
        label: "Review",
        prompt: "Review all the changes you made. If there is a REVIEW or REVIEW.md \
                 file in the project root, follow its checklist and criteria. Otherwise \
                 check for bugs, missing edge cases, security issues, and style \
                 inconsistencies. Summarize what you found and fix any issues.",
    },
    QuickPrompt {
        key: '2',
        label: "Commit, push & PR",
        prompt: "Commit all your changes with a clear, conventional commit message. \
                 Choose a descriptive branch name matching the work done if not already \
                 on one. Push the branch and create a pull request (or update it if one \
                 already exists). Write a PR description summarizing the changes, \
                 motivation, and what was tested.",
    },
    QuickPrompt {
        key: '3',
        label: "Run tests",
        prompt: "Run the project's test suite and fix any failures. If there are no \
                 tests for the code you changed, write appropriate tests first, then \
                 run them.",
    },
    QuickPrompt {
        key: '4',
        label: "Lint & format",
        prompt: "Run the project's linter and formatter. Fix all warnings and errors. \
                 If no linter is configured, use the language's standard tools.",
    },
    QuickPrompt {
        key: '5',
        label: "Explain changes",
        prompt: "Give me a concise summary of every change you made: which files, what \
                 was changed, and why. List any decisions you made and their trade-offs.",
    },
    QuickPrompt {
        key: '6',
        label: "Continue",
        prompt: "Continue working on the task. Pick up where you left off.",
    },
    QuickPrompt {
        key: '7',
        label: "Commit only",
        prompt: "Stage and commit all changes with a clear, conventional commit message. \
                 Do not push or create a PR yet.",
    },
    QuickPrompt {
        key: '8',
        label: "Undo last change",
        prompt: "Undo the last change you made. Use git to revert the last commit if \
                 committed, or restore the files if uncommitted. Explain what was undone.",
    },
];
