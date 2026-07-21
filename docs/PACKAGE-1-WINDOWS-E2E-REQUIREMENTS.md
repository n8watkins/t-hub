# Package 1 Windows E2E Requirements

This document defines the packaged Windows actual-dialog verification required before Package 1 is released for review.

The test must use the packaged or installed Windows T-Hub application and the real Captain commission dialog.

The test must not replace the WSL picker, IPC transport, filesystem adapter, or control daemon with mocks.

Set `T_HUB_DISTRO` to the distro under test and record the exact value in the test evidence.

Register a populated non-Git WSL directory and verify that the dialog shows the selected absolute POSIX root, the explicit codebase name, and a non-Git capability.

Commission a Captain from that registration and verify that no `.git` directory is created and that registration and commission use the same Project identity.

Register an empty non-Git directory and verify that the picker reports a loaded empty state rather than a directory-list failure.

Register a valid Git directory and verify that the existing Project ID, Captain bindings, Git main-root metadata, and default branch survive restart and recovery.

Exercise `/home/natkins/appturnity/monorepo-app` and verify that the Windows host does not reject the POSIX root as non-absolute.

Cause a directory-list failure and verify that the dialog reports an error state distinct from an empty directory.

Return responses out of order and verify that stale picker results cannot replace the newest selection.

Submit a foreign WSL distro UNC path, a file, a traversal path, a nonexistent path, an unauthorized remote path, and a symlink-equivalent path.

Verify that invalid roots are rejected before persistence and that equivalent real paths produce one Project under concurrent registration.

Invoke Git-only operations for a registered non-Git Project and verify the stable `git_required` error and absence of filesystem, worktree, board, delivery, or capacity mutation.

Invoke the explicit Git initialization operation and verify that it alone creates `.git`, upgrades the persisted capability, and preserves the Project ID and Captain bindings.

Capture the visible dialog, request payloads, persisted snapshot, filesystem state, and structured errors for every case.

This document is preparation only.

The packaged Windows run, installation, packaging, and release remain intentionally unexecuted until separately authorized.
