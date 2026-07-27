# Package boundaries

## The allowed dependency DAG

Arrows point from the depended-upon toward the depender: a package may use
anything to its left and nothing to its right.

    luther-engine-core <- generic components <- software-change <- github <- app
    daemon-core        <- work-source connectors <- app

Only the first element of the first row exists as a package today. The
remainder are named so the direction is settled before the code arrives,
rather than being decided one move at a time by whichever import compiles.

## Why this is a package and not a module

A module boundary is a naming convention. Nothing prevents a module from
importing its parent, and nothing fails when it does. The previous attempt at
this separation was declared in documents and asserted in review, and it did
not hold — the postmortem in
`research/creature/archive/main/notes/luther-mvp-attempt-1/overview.md:59-77`
records the boundary as never mechanically enforced.

A package boundary is enforced by Cargo. `luther-engine-core` cannot import
from `luther-workflow`, because the dependency runs the other way and Cargo
rejects the cycle outright. That rejection is the enforcement; the tests below
cover what Cargo's cycle check does not.

## Forbidden vocabulary in core

These names must not appear in `luther-engine-core`, including in comments:

- GitHub
- issue
- pull request
- CodeRabbit
- LLxprt
- branch
- merge strategy
- remediation
- scope policy

Comments are included deliberately. A primitive whose documentation explains
itself in terms of pull requests carries domain knowledge in its rationale even
when its types do not, and that is the route by which the concept returns: the
next maintainer reads the comment and writes code to match it.

## What enforces this

`workflow/tests/package_boundary.rs`, four tests reading the real dependency
graph and the real source rather than a declaration:

| Test | Fails when |
| --- | --- |
| `core_has_no_dependency_on_any_domain_package` | `cargo tree` for core shows an edge to a domain package |
| `the_domain_package_does_depend_on_core` | the edge is absent, which would make the test above vacuous |
| `core_source_contains_no_domain_vocabulary` | a forbidden name appears in core's source or comments |
| `core_manifest_names_no_workspace_member` | core's manifest references anything in this workspace |

Each was verified to fail when the property it protects is broken:

    add a dependency on a workspace member    2 tests fail
    write "pull request" into core's source   1 test fails
    remove the domain -> core edge            the build breaks
    make core depend on its own parent        Cargo rejects the cycle

The second test exists because the first can be satisfied by two packages that
never reference each other. A boundary between two unrelated things is not a
boundary, and a test that cannot tell the difference is decoration.

## Current contents of core

`sha256_hex` only. It qualifies on the evidence rather than on intent: zero
domain references, no dependency on any other module in the crate, and five
existing callers spread across persistence, recovery, and the tool contract.

The bulk of the executors, the schema split, and registry composition are
explicitly out of scope here and are handled by B2 through B6. This change
establishes the boundary and proves it holds; it does not relocate the code
that will eventually sit behind it.

## Known accepted violation

`StepOutcome::Fixable` is documented as "the issue is fixable by remediation"
(`src/engine/transition.rs:25-29`) — domain semantics on a core type. It is
deliberately left alone: correcting it requires the output-port design from B8
and would turn a move-only change into a semantic one. `StepOutcome` has not
moved into core, so the forbidden-vocabulary test does not yet apply to it.
