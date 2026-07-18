# G-FEAT Pre-Registration: Session Rejection Memory (defect B)

Status: pre-registered before verification. Scope: SmartKey local input method only.

## Problem

SmartKey shows a ghost completion as the user types. Today the commonest form
of rejection — the user simply **typing through** the ghost, or committing the
word at a boundary without accepting it — is only written to the local replay
log and is never fed back into the engine. The same unwanted completion keeps
re-appearing for the same typed input, forcing the user to reject it every time.

## Feature

When the user rejects the **same** ghost completion for the **same** typed
input **K = 2** times within a single session, on the **3rd** time they type
that input the same completion is **not offered again** for the rest of the
session.

- Keyed by `(typed prefix, completion)`, both normalized (trimmed, lowercased).
- A rejection is any of: typing through / diverging from the ghost, committing
  the word at a boundary while the ghost was still pending, accepting-then-
  deleting (frustration REJECT), or dismissing with Escape then typing
  (frustration ABANDON). Manually typing the completion out is **not** a
  rejection and is explicitly excluded.

## Falsifiable acceptance criteria

1. **K = 2 semantics.** For a fixed `(prefix, completion)`: after exactly 1
   recorded rejection the completion is still offered; after exactly 2 it is
   suppressed. (`RejectionMemory::is_suppressed` returns `false` at count 1 and
   `true` at count ≥ 2.) Verified by `rejection_memory.rs` unit tests and the
   `master_loop.rs` integration test
   `rejection_memory_suppresses_ghost_after_two_rejections`.
2. **Per-key independence.** A different typed prefix, or a different completion
   for the same prefix, does **not** inherit suppression. Verified by
   `different_prefix_not_suppressed` and
   `different_completion_same_prefix_not_suppressed`.
3. **Session scope / decay.** State is held only in the running engine process
   and is **never** serialized into `PersonalProfile`. A fresh engine process
   starts with an empty rejection memory. Verified by
   `fresh_instance_is_session_clean` and by the deliberate absence of any
   `RejectionMemory` field in `personal.rs` serialization.
4. **Bounded memory.** At most 256 distinct prefixes are retained; the oldest
   (least-recently recorded) prefix is evicted beyond that. Verified by
   `lru_eviction_keeps_within_capacity`.
5. **No false suppression.** Typing the completion out by hand (matching the
   ghost) is never recorded as a rejection, so good completions are not
   suppressed. Enforced by the divergence check in the adapter's
   `_is_genuine_ghost_rejection`. **Telemetry alignment:** the replay log also
   honors this — a manually completed ghost is written as event `completed`
   with reason `completed_manually`, never as `rejected`; only a genuine
   divergent/boundary rejection is logged `rejected` and fed to the memory.
6. **One record per rejection.** A single user rejection produces at most one
   recorded count. The frustration cache (`last_shown_ghost`) is consume-once
   and is cleared on any derived record and on typed-through boundary commits,
   so a boundary rejection cannot be re-counted by a later ABANDON. Verified by
   `word_boundary_record_then_abandon_counts_once`.

Punctuation word-boundaries (e.g. `.` `,` `!`) that commit the current word are
in scope of criterion 1's "type through / word boundary" and are recorded on the
same footing as Space/Return when they flow through the divergence guard.

## No-LLM-in-loop statement

This feature contains **no LLM in the loop**. It is a deterministic in-memory
counter over `(prefix, completion)` pairs with a fixed integer threshold and a
fixed LRU cap. No model, no network call, no inference is involved in recording
or in the suppression decision.

## DIANA-neutral statement

**DIANA-neutral: local input-method UX only.** This change affects only the
on-device SmartKey input method's ghost-completion behavior for the current
user session. It makes **no** networked, fleet-wide, or cross-device claims;
it collects, transmits, or aggregates **no** data; and it has no persistence
beyond the lifetime of the local engine process.
