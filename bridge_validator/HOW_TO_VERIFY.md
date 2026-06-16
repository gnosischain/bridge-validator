# How to Verify a Published Bridge Validator Image

Run checks 1 **and** 2 on every release. Run check 3 when you want
to trust nothing but the source. For the full reasoning, see
[`VERIFICATION_DETAILS.md`](./VERIFICATION_DETAILS.md).

## 1. Digest check

> Checks that the `<VERSION>` tag on Docker Hub still points to the exact
> image CI recorded for this release — i.e. the tag has not been re-pointed
> since publication.

Take `RECORDED_DIGEST` from the GitHub Release body (the `digest:` line under
**Published image**), then compare it to what the registry serves now:

```bash
docker buildx imagetools inspect gnosischain/bridge-validator:<VERSION> \
  --format '{{.Manifest.Digest}}'
```

Output equals `RECORDED_DIGEST` → pass. Mismatch → the tag was re-pointed;
**stop and investigate.**

**Example output for v0.1.3**

```bash
% docker buildx imagetools inspect gnosischain/bridge-validator:v0.1.3 --format '{{.Manifest.Digest}}'

Name:      docker.io/gnosischain/bridge-validator:v0.1.3
MediaType: application/vnd.oci.image.index.v1+json
Digest:    sha256:5e08ce065ea2edfd2e9aee3d798c0523dd2a1ebba4d201247e55d13e4b7cb75a # <--- Should match this digest

Manifests:
  Name:        docker.io/gnosischain/bridge-validator:v0.1.3@sha256:a278b5a4ca39230ad77f3f2c461a484f77137079530120bbe38cb4310866d193  # <--- amd64 image (referenced by attestation below)
  MediaType:   application/vnd.oci.image.manifest.v1+json
  Platform:    linux/amd64

  Name:        docker.io/gnosischain/bridge-validator:v0.1.3@sha256:14f411bc6c840e81fb4f517f33f36d467c409582193b5f865cc44048465bbd8f # <--- Needed for cosign verify (amd64 attestation)
  MediaType:   application/vnd.oci.image.manifest.v1+json
  Platform:    unknown/unknown
  Annotations:
    vnd.docker.reference.digest: sha256:a278b5a4ca39230ad77f3f2c461a484f77137079530120bbe38cb4310866d193
    vnd.docker.reference.type:   attestation-manifest

  Name:        docker.io/gnosischain/bridge-validator:v0.1.3@sha256:5ec3d99ce31698bb7b22c108e9e0e971bddc004162d71899af6e00b8b48e20b8 # <--- arm64 image (referenced by attestation below)
  MediaType:   application/vnd.oci.image.manifest.v1+json
  Platform:    linux/arm64

  Name:        docker.io/gnosischain/bridge-validator:v0.1.3@sha256:2c3618855dde6994cba746cadca53d8f249cad27d7d0f763cceaeee8285918f4 # <--- Needed for cosign verify (arm64 attestation)
  MediaType:   application/vnd.oci.image.manifest.v1+json
  Platform:    unknown/unknown
  Annotations:
    vnd.docker.reference.digest: sha256:5ec3d99ce31698bb7b22c108e9e0e971bddc004162d71899af6e00b8b48e20b8
    vnd.docker.reference.type:   attestation-manifest
```

## 2. Signature check with cosign

> Checks that the image was built and signed by Docker's `github-builder`
> workflow on GitHub Actions — anchored in the public Sigstore transparency
> log — so a forged Release body or a compromised registry cannot fool you.

Requires [`cosign`](https://docs.sigstore.dev/cosign/system_config/installation/).
The signatures live on the per-platform **attestation manifests**, not on the
index — so verify against each `ATTESTATION_DIGEST` (the `attestation-manifest`
entries you read out of the index in §1), never the tag and never the index
digest.

Fetch the attestation manifest hashes from the §1 output.

**Example for v0.1.3**

1. linux/amd64: `sha256:14f411bc6c840e81fb4f517f33f36d467c409582193b5f865cc44048465bbd8f`
2. linux/arm64: `sha256:2c3618855dde6994cba746cadca53d8f249cad27d7d0f763cceaeee8285918f4`

```bash
cosign verify \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp '^https://github.com/docker/github-builder/.github/workflows/build.yml.*$' \
  --certificate-github-workflow-repository gnosischain/bridge-validator \
  gnosischain/bridge-validator@sha256:<ATTESTATION_DIGEST>
```

Success prints the verified claims and the signing certificate's identity.
The `--certificate-github-workflow-repository` flag pins the calling
repository cryptographically — the signature then proves the image was built
by `github-builder` _invoked from this repo_.

Note: only releases built by the github-builder pipeline are signed; older
tags (v0.1.1 or less) fail with "no matching signatures".

**Example output for v0.1.3** (linux/amd64 attestation)

```bash
% cosign verify \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp '^https://github.com/docker/github-builder/.github/workflows/build.yml.*$' \
  --certificate-github-workflow-repository gnosischain/bridge-validator \
  gnosischain/bridge-validator@sha256:14f411bc6c840e81fb4f517f33f36d467c409582193b5f865cc44048465bbd8f

Verification for index.docker.io/gnosischain/bridge-validator@sha256:14f411bc6c840e81fb4f517f33f36d467c409582193b5f865cc44048465bbd8f --
The following checks were performed on each of these signatures:
  - The cosign claims were validated
  - Existence of the claims in the transparency log was verified offline
  - The code-signing certificate was verified using trusted certificate authority certificates

[{"critical":{"identity":{"docker-reference":"index.docker.io/gnosischain/bridge-validator@sha256:14f411bc6c840e81fb4f517f33f36d467c409582193b5f865cc44048465bbd8f"},"image":{"docker-manifest-digest":"sha256:14f411bc6c840e81fb4f517f33f36d467c409582193b5f865cc44048465bbd8f"},"type":"https://sigstore.dev/cosign/sign/v1"},"optional":{}}]
```

Repeat with the `linux/arm64` attestation digest (`sha256:2c3618…`); it prints
the same three checks and a matching claims block for that digest.

## 3. Full audit: rebuild from source

> Checks that the published image's application payload (`/app`: the `worker`
> binary + `migrations`) is byte-identical to what the source at the release
> commit builds — without trusting CI at all. Both published platform slices
> (`linux/amd64` and `linux/arm64`) are verified; the non-native one runs
> under emulation and is slow (often 30+ min for a Rust release build).

### Inputs you need (obtain from a trusted, out-of-band channel)

- `VERIFIER_SHA` — commit SHA of the audited `verify.sh` to run (the tool).
- `VERSION` — release tag to check, e.g. `v0.2.0` (the subject).
- `EXPECTED_SOURCE_COMMIT` — commit SHA `VERSION` must resolve to.

### Prerequisites

- `docker` (with `buildx`), `git`.
- Network to Docker Hub and `github.com`; a few GB free disk.
- Emulation for the non-native slice (Docker Desktop: Rosetta/QEMU), or set
  `PLATFORMS` to verify only your native slice.

### Steps

1. Download the tool, pinned to its commit:

   ```bash
   curl -fsSL \
     https://raw.githubusercontent.com/gnosischain/bridge-validator/<VERIFIER_SHA>/verify.sh \
     -o verify.sh
   ```

2. Run it against the release, asserting the expected source commit:

   ```bash
   bash verify.sh <VERSION> <EXPECTED_SOURCE_COMMIT>
   ```

   To verify a single platform only (e.g. native-only, no emulation):

   ```bash
   PLATFORMS=linux/arm64 bash verify.sh <VERSION> <EXPECTED_SOURCE_COMMIT>
   ```

### Result

- `✅ VERIFICATION PASSED` → image at `<VERSION>` was built from
  `<EXPECTED_SOURCE_COMMIT>`; `/app` is byte-identical on every verified slice.
- `❌ VERIFICATION FAILED` → differences under `/app`, or the tag does not
  resolve to `<EXPECTED_SOURCE_COMMIT>`. Do not deploy; investigate.

Note: only tags cut after the base images were pinned by digest in
`bridge_validator/Dockerfile` are expected to reproduce; older tags had
floating `FROM` lines (see
[`VERIFICATION_DETAILS.md`](./VERIFICATION_DETAILS.md)).
