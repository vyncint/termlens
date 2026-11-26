# Fixture: a single-quoted HTML `src` whose target is not in the repository

The gate read only double-quoted attributes, so this one passed unchecked (#859).

<img src='missing.png' alt=''>
