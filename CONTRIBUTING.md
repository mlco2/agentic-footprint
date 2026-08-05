# Contributing

Start with [`docs/contributing/developer-guide.md`](docs/contributing/developer-guide.md).

Before opening a change:

```sh
cargo test -p af-cli --test cli
scripts/test-install.sh
cargo fmt --all -- --check
git diff --check
scripts/check-repository-hygiene.sh
scripts/docs.sh build
```

Use the narrower package/collector tests listed in the developer guide while
iterating. Follow
[`docs/contributing/api-boundaries.md`](docs/contributing/api-boundaries.md)
for crate ownership and
[`docs/contributing/error-handling.md`](docs/contributing/error-handling.md)
for runtime failure behavior.

By participating, contributors agree to follow [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
