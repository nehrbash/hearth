# PAM review boundary

The review covers the Rust supervisor, native conversation callback, Scheme
controller and phase/outcome publisher. A separate helper wrote private PAM
fixtures and reviewed the implementation; the primary agent inspected the
protocol, secret handling and process teardown. This is a project review, not
an independent security certification or approval to deploy a lock screen.

Review findings addressed before the milestone gate:

- Drain the private socket through EOF before accepting the child's result.
  Require both a typed result and successful process exit.
- Avoid buffered-reader copies of responses; zeroize owned protocol buffers.
- Kill the attempt when its supervisor dies. Keep the exited leader unreaped
  until process-group cleanup so its group ID cannot be reused.
- Kill module helpers on normal completion as well as cancellation and timeout.
- Handle controller termination through cleanup, cancelling attempts and
  closing the publisher so Weft records unavailability.

Private fixtures exercise authentication and account denial, identity changes,
all conversation styles, multiple prompts, replayed replies, missing policy
despite a permissive fallback, module crashes, hangs, output backpressure and
helper termination. Module stdout/stderr cannot supply protocol results or leak
responses through the worker's output.

PAM modules remain trusted native code and can perform policy-defined external
effects. Guile and PAM may retain response copies; memory erasure is not
guaranteed. A same-user process can forge its own authentication service; worker
results are not authority for another process. Secure physical keyboard capture,
Unicode input and compositor session-lock ownership require separate integration
and review. No real authentication or session-lock action was exercised.
