# Fixture CONTRIBUTING.md

## 1. Dev setup

```sh
git clone https://example.invalid/termlens
cd termlens
```

```sh
cat > /dev/null
true
false
```

## 2. Project layout

Fixture for tools/preflight-selftest/run.sh (#719): the first gate reads its
standard input to the end. The runner must still run the two below it,
report all three, and exit non-zero for the one that fails.
