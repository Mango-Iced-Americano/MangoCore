# MangoCore Repository Rebaseline

## 1. Plan identity and current state

This is the canonical, compaction safe implementation plan for the MangoCore
repository rebaseline. It is an execution plan, not a completion report.

### Observation baseline

- Observation commit: `883f73c2`.
- Active branch: `chore/clean-up`.
- The observation baseline follows four independent cleanup commits:
  `0c446a77`, `40d86b2f`, `6c4c69e3`, and `883f73c2`.
- The current core rebaseline work must be treated as unstaged or untracked
  candidate work until each change has its own review and acceptance proof.
- Candidate work is not part of the committed facts, does not prove the
  rebaseline, and must not be silently folded into a foundational commit.
- No build, QEMU run, lint, CI run, or repository purity check has been
  performed for this documentation-only Phase 0 task.
- The Phase-0 documentation commit allowlist is exactly these four files:
   `docs/plans/repository-rebaseline.md`,
  `docs/architecture/2026-07-18-mangocore-contract-map.md`,
  `docs/architecture/2026-07-18-mangocore-contract-matrix.yaml`, and
  `docs/architecture/2026-07-18-verify-contract-map.sh`. No source, build
  script, Docker, CI, submodule, toolchain, or boulder-state file is allowed
  in that commit. `docs/Work_Log` and all evidence remain explicitly excluded.

### Canonical boot profile vocabulary

Use these six boot profile names everywhere in this plan and in the future
implementation: `normal`, `competition`, `regression`, `ktest`,
`development`, and `debug`. `debug` is a development variant with additional
diagnostics; it is not a replacement for `development` and must not become a
seventh profile or alter competition behavior. Build mode `debug` may still be
mentioned when discussing compiler outputs, but it must be labeled as a build
mode rather than a separate boot profile.

### Known baseline behavior that must be proven, not assumed

- At `883f73c2`, root `make all` cleans before building and therefore is not an
  incremental contract.
- The baseline build can mutate source-adjacent generated files, vendor state,
  and configuration. The exact mutation set must be captured in Phase 0.
- The baseline may mutate or recreate generated language items, linker inputs,
  initramfs inputs, Cargo configuration, and lwext4 integration state. No one
  may call the build source-pure until those paths are measured.
- Existing output, image, boot, PID 1, lint, CI, and test behavior is observed
  behavior only. It is not an approved future contract.

## 2. Immutable rebaseline contract

The following requirements are binding for implementation. A later decision
may narrow or clarify a requirement only through the decision ledger and a
plan revision. It may not silently weaken the contract.

### Repository and review boundaries

1. Work from the existing `develop` lineage on the independent
   `chore/clean-up` branch. Preserve the four cleanup commits and do not mix
   them with foundational rebaseline commits.
2. Never rewrite history, force-push, or collapse the work into one review
   hostile commit.
3. Keep one concern per commit. Every behavior change must be independently
   reviewable and revertible.
4. Never treat unstaged or untracked candidate work as committed fact.
5. Do not create permanent `legacy/`, `old/`, or `backup/` graveyards.
6. Do not rebase MangoCore source onto DragonOS or Linux. They are reference
   implementations only.

### Root build contract

1. Preserve root `make all` as the official dual architecture entrypoint.
2. `make all` builds RV64 first and LA64 second, serially, from one declared
   manifest. It must fail if either architecture fails.
3. It must be incremental. It must not begin with an unconditional clean.
4. It stages complete per-architecture outputs before publishing root
   compatibility artifacts. A failed second stage must not publish a mixed
   generation of root artifacts.
5. A successful build leaves `git status --short` clean relative to the
   committed inputs. Build output and cache state are ignored or outside the
   checkout, but source, vendor, configuration, linker files, generated
   language items, and checksum files remain unchanged.
6. Normal build, run, and test targets never call rustup mutation, setup,
   downloads, network provisioning, hidden fallback commands, or `|| true`.
7. Provisioning is explicit and separate. `make preflight` is read-only.
   `make setup` or its approved equivalent may provision a declared
   environment, but formal build and test targets do not invoke it.

### Toolchain contract

1. Commit one dated nightly in root `rust-toolchain.toml`, with components and
   targets selected only after a clean environment audit.
2. Normal commands consume that toolchain. They do not call
   `rustup override`, `rustup default`, `rustup target add`, or
   `rustup component add`.
3. The first pin is the current supported RV64 and LA64 target arrangement.
   The LA64 bare-metal target migration is a separate decision and is not
   smuggled into nightly unification.
4. LLVM tooling is a separate audit item. It is not added merely because a
   build script currently downloads or discovers it.
5. The official Docker image, digest, Rust channel, components, targets, QEMU,
   binutils, CMake, and debugfs availability are recorded before the pin is
   accepted.

### Output and source-purity contract

1. All reconstructible target, image, log, download, and cache outputs live
   under `build/`, separated by architecture, build mode, and test or boot
   profile.
2. Root compatibility artifacts are publication outputs only. They are
   emitted after both architecture stages succeed.
3. `clean` removes only regenerable state. It does not restore or rewrite
   tracked source, vendor content, Cargo configuration, linker scripts,
   language items, or checksum files.
4. Cargo configuration, linker scripts, language-item selection, initramfs
   generation, and lwext4 build products are supplied through declared
   out-of-tree inputs or committed non-mutating integration.
5. Build purity means no forbidden mutation during the command, not merely a
   matching final checksum after a restore step.

### Competition, image, and QEMU contract

1. First freeze the official Docker and build entrypoints, final artifact names
   and locations, network and toolchain availability, QEMU arguments, drive
   order, partition metadata, and permitted permanent disk count.
2. Retain the official two-drive ABI. `x0` is evaluator-provided input and
   `x1` is the project tools or scratch image, subject to the profile matrix
   established in Phase 0. Do not require a permanent third formal disk.
3. Normal build never mutates evaluator-provided `x0`. Test configuration
   injection creates a named derived copy or explicit test image.
4. Use labels, UUIDs, partition metadata, or one central role map instead of
   scattered fixed device-number policy.
 5. Normal, competition, regression, ktest, development, and debug profiles
    have explicit launch contracts. Regression is the documented zero-disk
    initramfs profile. Debug is the development variant, not a replacement for
    development.
6. The final boundary is strict: build system reproduces, image tooling
   supplies file contents, kernel supplies mechanisms, PID 1 supplies system
   policy, tests verify behavior, and documentation describes the live system.

### Boot and PID 1 contract

1. Converge normal boot on a documented, self-contained initramfs bootstrap.
   Keep regression as an explicit zero-disk profile.
2. `/sbin/init` is minimal. It mounts required runtime filesystems and official
   disks, prepares the baseline environment, starts the selected runner,
   reaps children, and handles reboot or shutdown.
3. Competition and LTP scheduling, timeouts, exclusions, profiles, test-group
    data, and smoke logic belong to a separate runner for competition and ktest.
4. A transitional `/initproc` shim may exist only as a thin compatibility
    wrapper with an owner and removal gate. It may not duplicate PID 1 logic.
 5. In competition and ktest profiles, a missing runner or invalid runner
   configuration is fatal. It emits `MANGO_RUNNER_FAILURE`, reaps children,
    requests shutdown, and produces a nonzero test result. A rescue shell is
    allowed only in the explicit `development` profile; `debug` is its
    diagnostic variant.
6. The kernel retains devfs, procfs, sysfs, tmpfs, block discovery, partition
   parsing, mount syscall, and minimum console/bootstrap mechanisms. It does
   not hardcode normal `/dev`, `/proc`, `/sys`, `/run`, `/tmp`, `/dev/shm`, or
   disk mount policy once the PID 1 path is ready.

### Quality, CI, and documentation contract

1. Establish warning facts for RV64 and LA64, each in debug and release.
   Distinguish first-party code, maintained dependencies, and third-party
   vendor warnings.
2. Address high-risk lints first. Replace broad crate allows with justified
   local `#[expect(..., reason = ...)]` or real fixes.
3. Add non-bypassable `make check`, `make lint`, and CI gates. They reject new
   first-party warnings and source-purity, dual-architecture, and setup
   contract violations. They do not hide failure behind `|| true`.
4. Update stale claims that `cargo test` or `cargo clippy` are unavailable.
   The actual applicable first-party tests and lint paths must be documented,
   while bare-metal limitations remain explicit.
5. Synchronize README, AGENTS, architecture, testing, Docker, CI, script
   comments, image, boot, PID 1, mount-policy, warning-policy, and migration
   documentation with the live implementation.

## 3. Phase 0 through Phase 6

### Phase 0: Freeze facts and boundaries

**Goal:** Create the committed map that separates observed baseline facts,
candidate work, external contracts, risks, decisions, and acceptance rules.

1. Inventory root and nested Make targets, scripts, Docker entrypoints, CI
   jobs, submodules, toolchain declarations, output names, image builders,
   QEMU profiles, judges, test runners, and documentation claims.
2. Build the call graph and boot graph from build entrypoint through image
   construction, QEMU, kernel bootstrap, PID 1, runner, and shutdown.
 3. Freeze an image and disk table for normal, competition, regression, ktest,
    development, and debug profiles. Record x0, x1, partition or label
    identity, mount target, mutation rule, and expected artifact. Debug is the
    development variant and shares its disk contract unless explicitly
    documented otherwise.
4. Measure the baseline at `883f73c2`, including the fact that `make all`
   cleans and mutates source, vendor, or configuration. Do not repair the
   mutation during characterization.
5. Record RV64 and LA64 debug and release artifact facts, warning facts, and
   boot markers. A failed or unavailable baseline is recorded as RED, never
   upgraded to a pass by inference.
6. Create a risk register, decision ledger, deletion ledger, and acceptance
   matrix. Mark every unknown with an owner and exit condition.

**Exit gate:** The map names every public command and consumer, every final
artifact, every disk role, every boot profile, every candidate path, and every
unknown. No implementation change begins before this gate is reviewed.

### Phase 1: Pin and prove the environment

1. Audit the official Docker image and select one dated nightly with required
   components and current architecture targets.
2. Commit the root toolchain contract and a read-only preflight. Keep setup
   explicit, repeatable, and separate from formal commands.
3. Verify fresh clone and submodule behavior without altering the repository.
4. Test RV64 then LA64 in the same declared environment, serially. If a
   candidate fails, record the exact blocker and do not add compatibility
   branches or unrelated workarounds.
5. Record LA bare-metal target migration as deferred unless a separate plan is
   approved. Record LLVM tooling as a separate audit decision.

**Exit gate:** A fresh official environment can prove the pinned channel,
components, targets, and current dual-architecture build prerequisites without
formal build commands mutating rustup state or downloading inputs.

### Phase 2: Make the build source-pure and incremental

1. Move Cargo target state, linker selection, generated language items,
   initramfs inputs, and lwext4 build state out of source and vendor trees.
2. Use target-specific declared configuration or build inputs. Never directly
   edit generated `lang_items.rs`, its architecture variants, linker scripts,
   vendor files, or checksum files during a build.
3. Isolate `build/<arch>/<profile>/` outputs and caches. Make dependency inputs
   explicit and incremental.
4. Replace unconditional clean, build-time sed, copy, touch, restore, and
   hidden fallback behavior with real dependency edges or committed patches.
5. Run clean to incremental, repeated, reverse-order, debug to release, and
   invalid-input scenarios. Keep RV64 and LA64 commands serial.

**Exit gate:** Repeated builds reuse outputs, changing an input rebuilds only
the dependent product, and forbidden source, vendor, config, rustup, and
ignored source-adjacent state remains unchanged.

### Phase 3: Canonicalize products, images, and QEMU

1. Replace the nested build graph with a shallow root interface and focused
   architecture, kernel, user, image, run, and test responsibilities.
2. Preserve `make all` and only evidence-backed compatibility aliases. Aliases
   have deprecation text and no duplicate implementation.
3. Make normal initramfs, regression initramfs, development rootfs, and x1
   tools or scratch image manifest-driven. Required files fail the build when
   absent. Optional files are explicit.
4. Separate download and cache setup from image construction. Image builders
   are idempotent and clean their own temporary mounts.
5. Centralize QEMU profiles and the two-drive ABI. Reject swapped, missing, or
   forbidden drives before launch. Never scatter `/dev/vdb2` style assumptions.
 6. Make test configuration injection operate on a derived `development` image
    and preserve the official x0 input checksum.

**Exit gate:** All required outputs and images are reproducible from manifests,
all profiles have machine-readable launch contracts, and no official input is
mutated by normal build.

### Phase 4: Converge bootstrap and split PID 1

1. Make normal and regression initramfs payloads self-contained, with exact
   `/sbin/init`, runner, rescue, configuration, library, and helper contents.
2. Audit `initramfs`, `regression_initramfs`, `legacy_block_root`, `block_mem`,
   preload payloads, preload assembly, load and flush behavior, and fallback
   startup paths. For each path choose retain with invariant, remove after
   proof, or defer with owner and exit condition.
3. Preserve kernel bootstrap mechanisms while removing normal mount policy from
   kernel code after the userspace path is ready.
4. Split the minimal PID 1 from the competition and test runner. Preserve
   process lifecycle, reaping, shutdown, and fatal runner behavior.
5. Move pseudo-filesystem, runtime-directory, root, tools, and scratch mount
   ordering into PID 1 under the centralized role map.
 6. Verify normal, competition, regression, ktest, development, and debug
    behavior on both architectures, including fork, exec, wait, filesystem,
    network, and shutdown smoke behavior. Debug must remain a development
    variant rather than replacing development.

**Exit gate:** Both architectures reach the documented PID 1 marker, expose
the required runtime mounts and disk roles, run the selected runner, and
report controlled failures for missing devices or runner payloads.

### Phase 5: Establish lint, tests, and CI gates

1. Record the four warning cells, classify ownership, and fix high-risk
   first-party warnings first.
2. Replace broad allows with local justified expectations or fixes.
3. Define `make check` and `make lint` with explicit architecture and profile
   scope. Keep vendor exceptions narrow and visible.
 4. Define test contracts for kernel tests, host-side first-party tests where
    applicable, QEMU smoke, normal and regression boot, ktest, competition
    groups, development, debug, and LTP. Do not claim a test is available until
    its command and result are verified.
5. Apply the same source-purity, toolchain, serial-order, artifact, boot, and
   warning gates in CI using a fresh official environment.

**Exit gate:** Local and CI gates fail on newly introduced first-party
warnings, invalid setup, source mutation, mixed artifact publication, or boot
contract regressions. No required failure is masked.

### Phase 6: Remove proven debt, synchronize docs, and close the baseline

1. Remove obsolete scripts, generated files, root artifacts, duplicate paths,
   preload paths, legacy roots, block-memory loaders, and fallback code only
   after reference, docs, CI, build trace, image, and dual-architecture boot
   checks prove the replacement is complete.
2. Delete one understood capability per commit. Each deletion has a replacement
   commit, owner decision, rollback command, and fresh verification result.
3. Synchronize all documentation and AI instructions with the live build, boot,
   image, mount, PID 1, test, lint, and CI contracts.
4. Run the final clean-container acceptance matrix: explicit setup if needed,
   read-only preflight, serial `make all`, expected artifacts, normal and
   regression boot, relevant tests, lint and CI-equivalent checks, and clean
   worktree.
5. Review every requirement in this plan. Any missing proof remains open and
   blocks the rebaseline declaration.

**Exit gate:** The repository reproduces the declared products and runtime
behavior from a fresh official environment, and every remaining compatibility
path has either been proven necessary or is documented with a bounded owner
and exit condition.

## 4. Risks and decisions

### Risk register

| Risk | Failure mode | Required control |
| --- | --- | --- |
| Build mutation | A successful build dirties source, vendor, Cargo config, or generated files | Capture before and after manifests, remove mutation at its owner, reject restore-after-build tricks |
| Shared architecture state | RV64 and LA64 overwrite each other or are run concurrently | Serial execution, isolated output roots, explicit architecture selection |
| Toolchain drift | Build silently installs or selects a different nightly or target | One dated pin, read-only preflight, explicit setup only |
| Mixed publication | RV64 output is published while LA64 failed | Stage both, publish only after both pass, preserve prior complete publication |
| Image corruption | Required payload is missing or evaluator x0 is changed | Manifests, checksums, derived test images, immutable official inputs |
| Disk ABI break | QEMU drive order or partition role changes | Phase 0 profile matrix, central role map, two-drive gate |
| Boot regression | PID 1 or mount policy moves before userspace support exists | Characterization first, incremental phase gates, dual-architecture QEMU |
| PID 1 scope creep | Test policy remains embedded or lifecycle is lost | Separate runner contract and fatal runner failure behavior |
| Unsafe deletion | An apparently unused compatibility path is needed by CI or evaluator | Search code, docs, CI, history, generated manifests, then fresh boot proof |
| Warning masking | Broad allows or ignored lint hide new defects | Ownership baseline, local expectations, non-bypassable check and lint |
| Evidence confusion | Temporary logs or old results are treated as current proof | No evidence-only commit, freshness review, explicit result metadata outside this plan |
| Context loss | A resumed executor repeats destructive work or invents completion | Recovery ledger, current commit and dirty-path snapshot, exact next command |

### Decision ledger

The executor must update this ledger only through a reviewed plan revision or
implementation decision record. The initial decisions are:

| Decision | Initial position | Approval condition |
| --- | --- | --- |
| Observation point | `883f73c2` | All baseline claims are labeled observed, candidate, or verified |
| Root entrypoint | Preserve serial `make all` | Both architecture stages and clean-worktree contract pass |
| Toolchain | One dated nightly | Fresh image audit proves required components and targets |
| LA bare-metal target | Deferred from this rebaseline | Separate ABI, linker, relocation, and boot plan |
| Competition disks | Exactly x0 plus x1 | Phase 0 confirms official roles and drive order |
| Normal bootstrap | Self-contained initramfs and documented PID 1 | Dual-architecture boot and mount matrix passes |
| Kernel mount policy | Mechanisms only, policy in PID 1 | PID 1 has required syscall and device support |
| Evidence | Evidence is not a product artifact and is never committed as a substitute for code proof | Review accepts current, attributable, fresh results |
| Compatibility deletion | Delete only after replacement and two independent clean verification cycles | Deletion ledger and rollback path complete |

## 5. Required scenarios

### Scenario A, source-pure reproducible build

From a fresh official Docker environment, run explicit setup if needed, then
read-only preflight and serial `make all`. Both architecture artifact sets and
required compatibility artifacts exist. The checkout is clean. A RED result is
any source, vendor, config, rustup, or generated-file mutation, unconditional
clean, missing artifact, or masked architecture failure.

### Scenario B, canonical normal boot

Each architecture starts its documented normal QEMU profile, reaches PID 1,
mounts `/dev`, `/proc`, `/sys`, writable `/tmp` and `/dev/shm`, mounts the
documented root and tools roles, can fork, exec, and wait, and shuts down.

### Scenario C, regression, development, and debug isolation

Regression uses the zero-disk initramfs profile. The `development` profile's
shell behavior is explicit and does not become competition policy. Debug is the
the `development` variant, not a replacement for development. Normal, competition, regression,
ktest, development, and debug outputs do not overwrite one another or dirty the
checkout.

### Scenario D, runner failure

In competition and ktest profiles, remove or invalidate the runner payload.
PID 1 emits `MANGO_RUNNER_FAILURE`, reaps children, requests shutdown, and the
test result is nonzero. Only the explicit development profile may enter rescue
behavior; debug remains its diagnostic variant.

### Scenario E, image and disk safety

A normal build leaves evaluator x0 unchanged. Derived test injection records
its own configuration and image identity. Swapped, missing, corrupt, or
third-drive inputs fail before QEMU starts.

### Scenario F, quality and CI regression

`make check`, `make lint`, and CI run the same declared architecture and source
purity contracts. A known first-party warning or invalid setup fixture causes
a nonzero result. Vendor warnings are classified rather than ignored.

## 6. Acceptance matrix

| Area | Required proof | Pass condition | Blocker |
| --- | --- | --- | --- |
| Repository map | Public target, consumer, artifact, profile, and owner inventory | No unknown without owner and exit condition | Missing external contract |
| Baseline | Observation at `883f73c2` | Cleaning and mutation behavior recorded as RED facts | Candidate work presented as baseline |
| Toolchain | Fresh official image audit | One dated nightly, components, targets, and explicit setup contract | Hidden download or rustup mutation |
| Dual architecture | RV64 then LA64, never concurrent | Both required stages pass with correct metadata | Either architecture fails |
| Incrementality | Repeated and input-change builds | Correct reuse and dependency rebuilds | Unconditional clean or broad rebuild |
| Purity | Worktree, vendor, config, rustup, generated-file comparison | No forbidden mutation during formal command | Restore-after-build workaround |
| Publication | Failure injection at second stage | No mixed-generation root artifacts | Partial publication |
| Images | Manifest, checksum, content, and idempotence checks | Required payloads present and official x0 unchanged | Silent missing file or input mutation |
| QEMU ABI | Normal, competition, regression, ktest, development, and debug profile table | Exact two-drive or zero-drive contract honored | Scattered device assumptions |
| PID 1 | Marker, mount table, runner, reaping, shutdown | Minimal policy owner passes both architectures | Test policy embedded in PID 1 |
| Failure path | Missing runner, invalid image, missing disk, bad config | Controlled nonzero result and diagnostic marker | Panic, hang, or masked failure |
| Tests | Kernel, QEMU, runner, relevant test groups, and LTP contracts | Applicable tests run with declared profile and result | Unverified command claim |
| Lint | Four warning cells and ownership baseline | No new first-party or high-risk warning | Broad allow or hidden output |
| CI | Fresh environment and same gates | CI rejects purity, setup, order, artifact, boot, and lint regressions | CI differs from local contract |
| Deletion | Reference and replacement ledger | Each removal has fresh dual-arch proof and rollback | Unbounded compatibility deletion |
| Documentation | README, AGENTS, docs, scripts, Docker, CI | Commands and responsibility statements match behavior | Stale instructions |
| Final state | Full matrix and clean checkout | All gates pass before declaration | Any missing or stale proof |

## 7. Commit sequence and no-evidence-commit policy

### Commit sequence

Keep the following dependency order. Split a row further when its review or
rollback boundary is not independent.

1. `docs(architecture): freeze repository and artifact contracts`
2. `build: add pinned unified Rust toolchain`
3. `build: isolate Cargo linker and initramfs outputs`
4. `build(lwext4): move integration builds outside dependency sources`
5. `build: define canonical serial artifact graph`
6. `image: define manifest-driven bootstrap and scratch artifacts`
7. `run: consolidate QEMU profiles and disk-role contracts`
8. `build(initramfs): package canonical bootstrap manifests`
9. `boot: separate device registration from mount policy`
10. `init: split PID 1 lifecycle from competition test runner`
11. `fs: move runtime mount policy to PID 1`
12. Separate `refactor(boot): remove verified obsolete <capability>` commits,
    one capability per commit.
13. `lint: establish dual-architecture warning baseline and gates`
14. `ci: gate dual-architecture purity and boot contracts`
15. `docs: synchronize rebaselined build, image, boot, and init contracts`

The four existing cleanup commits remain historical inputs. They are not
recreated, squashed, amended, or treated as proof for any row above.

### Explicit no-evidence-commit policy

- Do not commit evidence directories, raw logs, generated reports, temporary
  images, cache contents, container state, or evidence-only metadata as part of
  this rebaseline wave.
- Do not create a commit whose only purpose is to make an unverified claim look
  persistent.
- A commit is acceptable only when its implementation, tests, and review
  boundary are clear. Evidence may be retained by the approved project
  workflow outside the commit, but the plan must never use an old or missing
  result as completion proof.
- If required proof cannot be retained because of an environment limit, mark
  the gate blocked and name the missing fields. Do not claim success.
- Before any future commit, inspect status, diff, and history, stage only that
  concern, and stop if the commit would include unrelated candidate work.

## 8. Recovery after context compaction

At the start of every resumed execution turn:

1. Read this plan and the matching draft or execution ledger if one exists.
2. Treat this plan's checkboxes as scope, never as completion state.
3. Reconcile the current branch, observation commit, committed changes,
   unstaged paths, untracked paths, and latest verified result before running
   anything destructive.
4. Identify the current phase, current todo, last known-good commit, pending
   decision, failed gate, and exact next command.
5. Do not repeat a clean, migration, deletion, image rewrite, or toolchain
   provisioning step until its previous result is classified as current,
   stale, missing, or invalid.
6. If a result is stale or unavailable, rerun only that gate in the approved
   environment, serially for RV64 then LA64. Never infer success from a final
   checksum or a previous session message.
7. Before handoff or another compaction, write the execution ledger entry with
   current phase, todo, last known-good commit, committed and dirty paths,
   latest result state, freshness decision, deviation, blocker, and exact next
   command. Do not put evidence-only files into a code commit.

### Recovery stop conditions

Stop and return to Phase 0 when the active branch differs from the recorded
lineage, candidate work cannot be classified, a build mutates a new forbidden
path, the architecture order is ambiguous, a disk role is undocumented, or a
required gate has no attributable current result. These are planning or
contract failures, not reasons to add a workaround.

## 9. Completion statement

The rebaseline may be declared complete only when every acceptance matrix row
is green, both architectures pass the final fresh-environment sequence, root
`make all` is serial, incremental, source-pure, and clean, the normal,
competition, regression, ktest, development, and debug profiles match their
contracts, PID 1 and the runner are separate,
quality and CI gates are non-bypassable, obsolete paths are either proven
necessary or removed with rollback, and documentation describes the verified
live system.

Until then, this file records intended work only. In particular, the current
core rebaseline changes remain unstaged or untracked, unvalidated candidate
work and are not evidence that any phase has passed.
