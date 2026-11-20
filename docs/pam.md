# PAM authentication

Hearth's PAM service authenticates the current user's account through an
explicitly selected PAM policy. It is an authentication component, not a
session-lock implementation, and never issues a compositor unlock.

Every attempt runs in a separate child process. The supervisor enforces bounded
input, prompt identity, cancellation and deadlines independently of PAM modules.
Success requires both authentication and account authorization. Missing policy,
expired or denied accounts, child crashes, EOF and timeouts fail closed. There
is no automatic retry and no fallback to another policy.

The dedicated Scheme UI handles prompts and sends responses through the private
worker pipe. All entered responses are masked, including echo-on prompts, and
are cleared on prompt changes, cancellation and disconnect. No response buffer
is copied into scene nodes or semantic data. The channel is sensitive and
excluded from generic Weft mirroring. The explicit Weft publisher accepts only
phase and outcome, without usernames, prompts, response lengths or credentials.

Rust-owned response buffers are zeroized and core dumps are disabled. PAM modules
and Guile may hold temporary copies; this is not a guarantee of memory erasure.
The dedicated UI process is trusted credential-handling code. A normal session
user can run a different executable or private policy, so these results are not
transferable authorization tokens for another process or an owner effect.

Tests compile a private PAM module and use temporary configuration directories.
They exercise conversation styles, account denial, stale responses, crashes,
cancellation and deadlines without authenticating a real user or changing
`/etc/pam.d`. Fixture success never authorizes a real unlock. Desktop policy
installation and activation remain with the owner.

The UI supplies a paged on-screen keyboard for printable ASCII, bounded to 4096
bytes. Physical keyboard capture, arbitrary Unicode entry and compositor
session-lock integration are separate work. This panel cannot replace Burl's
lock screen.

Run the renderer with `hearth --listen SOCKET --channel pam --size 720x760`, then
the controller with `guile --no-auto-compile -L scheme scheme/pam-ui.scm SOCKET
hearth-pam --service NAME`. The service name is fixed at launch; authentication
starts only when the user presses the start button. Add `--publish-workspace`
before `SOCKET` to publish phase/outcome through a separate bounded publisher.
`--config-dir` selects a private test policy directory and installs nothing.

Reproduce isolated verification with `guix shell -m manifest.scm -- cargo test
--test pam_private --test pam_state --test pam_snapshot` and `guix shell -m
manifest.scm -- guile --no-auto-compile -L scheme tests/pam_ui.scm`.
