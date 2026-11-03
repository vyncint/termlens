# Fixture: the shapes CONTRIBUTING.md legitimately uses

Not packaged, so relative links are fine — as long as the target is here,
resolved against this file's own directory rather than the repository root.
The `../../../` is what the fixture's own depth costs it: against the root
the old rule read those as outside the repository.

- [the agent brief](../../../AGENTS.md)
- [releasing](../../../docs/RELEASING.md)
- [the workflow](../../../.github/workflows/ci.yml)
- [a fragment](#5-developer-certificate-of-origin-dco)
- [another project](https://github.com/vyncint/mossaic)
- [this project's demo](https://github.com/vyncint/termlens-demo)
- [a label](https://github.com/vyncint/termlens/labels/good%20first%20issue)
- [an external site](https://developercertificate.org)
- [a reference to a sibling][brief], defined below
- [a link with a title](../../../AGENTS.md "the brief")
- [a directory on GitHub](https://github.com/vyncint/termlens/tree/main/tools/link-gate-selftest)
- <a href="../../../docs/DESIGN.md">an HTML link</a> and <img src='../../../docs/STABILITY.md' alt=''>
- a footnote[^1], whose definition is prose and not a link target

[brief]: ../../../AGENTS.md
[^1]: Not a link target.
