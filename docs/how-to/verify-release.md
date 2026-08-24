# Verify a release

Each GitHub release contains platform archives, `SHA256SUMS`, Sigstore bundles,
and GitHub build provenance.

## Verify the checksum

```sh
sha256sum --check SHA256SUMS
```

On macOS, use `shasum -a 256 -c SHA256SUMS`.

## Verify the keyless signature

Install `cosign`, then verify an archive and its bundle:

```sh
cosign verify-blob \
  --bundle agentic-footprint-vX.Y.Z-aarch64-apple-darwin.tar.gz.sigstore.json \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp \
    'https://github.com/mlco2/agentic-footprint/.github/workflows/release.yml@refs/tags/vX.Y.Z' \
  agentic-footprint-vX.Y.Z-aarch64-apple-darwin.tar.gz
```

Replace the version placeholder with the published release version.
The signature is keyless: GitHub Actions obtains a short-lived identity through
OIDC, so the project does not keep a long-lived signing key in repository
secrets.

## Verify GitHub provenance

With the GitHub CLI:

```sh
gh attestation verify \
  agentic-footprint-vX.Y.Z-aarch64-apple-darwin.tar.gz \
  --repo mlco2/agentic-footprint
```

Verification proves which workflow and repository produced the artifact. It
does not replace reviewing the source tag or the project's security policy.
