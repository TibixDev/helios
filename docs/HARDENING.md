# KMD Hardening Tracker

Findings from the 2026-07-08 kernel review (`kmd/src` + `protocol` + the ICD
backend). No memory-corruption bugs were found; these are liveness and
multi-tenant isolation issues. Ordered as the fix roadmap. Checked items are
done (commit noted).

Sequencing rule: **H3 and H4 must be fixed before H5** — opening the device ACL
(H5) makes the H3/H4 abuse paths reachable by unprivileged callers.

## Stability (makes soak testing trustworthy)

- [x] ~~**H1 — residual `0x7F` kernel-stack pressure.**~~ Fixed: the
  `AdapterContext` now stores `Option<Box<VirtioGpu>>` (the ~2.1 KB `VirtQueue`
  lives on the heap, not inline), and the dead `FenceTable` (2.6 KB) is deleted
  along with `fence.rs`, `interrupt.rs`, and `wdf::interrupt_config`. The
  load-path stack temporary drops by ~4.7 KB. (commit)

- [x] ~~**H2 — unbounded busy-poll under the virtio spinlock at DISPATCH.**~~
  Fixed (step 1 — deadline bound): every used-ring poll now goes through
  `VirtioGpu::wait_can_pop`, which spins at most `HOST_POLL_BUDGET_SECS` (2 s, via
  `KeQueryPerformanceCounter`) and returns `DeviceError` on expiry instead of
  hanging at DISPATCH → no more `0x133` on a wedged host. `add_notify_wait_pop`
  (unbounded internal spin) is replaced by `add_wait_pop_bounded` at both control
  round-trips; `submit_venus` and `quiesce_into` use the bounded wait. (commit)
  **Follow-up (not yet done):** move the host wait off the DISPATCH spinlock to
  PASSIVE (as `WAIT_FENCE` does) so control ops don't serialize on one lock — a
  throughput/frame-pacing improvement, tracked below as H2b.

- [ ] **H2b — control round-trips still hold the virtio spinlock at DISPATCH
  across the (now bounded) host wait.** Correct and no longer bugchecks, but it
  serializes all IOCTLs and blocks preemption for up to the poll budget. Restructure
  so the lock guards only ring manipulation and the wait happens at PASSIVE.

## Non-admin enablement (bundle: H3 + H4, then H5)

- [ ] **H3 — uncatchable SEH on `MmMapLockedPagesSpecifyCache(UserMode)`**
  (`ioctl.rs`). Raises a structured exception on VA/quota exhaustion; `no_std`
  has no SEH, so it bugchecks. The 256 MiB size cap does not prevent it. Fix: a C
  `__try/__except` shim that returns NULL on raise (makes the existing `is_null`
  branch live). Mandatory before H5.

- [ ] **H4 — `MAP_BLOB` has no owner/ctx authorization** (`ioctl.rs`,
  `virtio/gpu.rs`). Blobs are looked up by `resource_id` alone across all clients
  (`RELEASE_BLOB` correctly checks `ctx_id` — the asymmetry confirms it). Any
  handle could map another client's host-visible blob. Fix: add `ctx_id` to
  `MAP_BLOB` and verify `blobs.get(ctx_id, resource_id)`. Mandatory before H5.

- [ ] **H5 — device ACL is admin/SYSTEM only** (no SDDL, `pnp.rs`). Non-admin
  RDP-session apps get `ACCESS_DENIED`. Fix (only after H3 + H4): assign a
  validated SDDL granting interactive users. Validate offline with
  `ConvertStringSecurityDescriptorToSecurityDescriptor` (the prior attempt failed
  with an invalid descriptor, Code 31).

## Cleanup (low)

- [ ] **L1 — window-offset leak on failed user-map** (`ioctl.rs`): bounded, and
  reclaimed at `release_blob`/`ctx_destroy`. Tidy alongside H3.
- [x] ~~**L2 — inert dead code**: `fence.rs` `FenceTable` and `interrupt.rs`
  skeleton.~~ Deleted with H1 (also removed the now-unused `wdf::interrupt_config`).
- [ ] **L3 — ICD `wait_fence_capacity` u32 multiply overflow**
  (`vn_renderer_helios.c`): user-mode, impractical; guard for tidiness.

## Verified NOT bugs (do not re-investigate)

Context-cleanup on abnormal client exit (already fixed: `ctx_destroy_for_owner`);
ICD↔KMD wire contract (matches byte-for-byte); `retired` Vec never reallocs under
the lock; `bytemuck` alignment; `SUBMIT_VENUS` TOCTOU (copies to a private buffer
at PASSIVE); cross-process unmap; MDL/PFN sizing.
