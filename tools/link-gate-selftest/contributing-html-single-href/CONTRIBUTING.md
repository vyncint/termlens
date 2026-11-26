# Fixture: a single-quoted HTML `href` whose target is not in the repository

The gate read only double-quoted attributes, so this one passed unchecked (#859).

<a href='missing.md'>a page</a>
