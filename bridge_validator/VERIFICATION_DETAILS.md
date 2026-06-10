# Verifying the Bridge Validator Image — Details

Reference for verifying that `docker.io/gnosischain/bridge-validator:<TAG>` was
built from this repo at git tag `<TAG>`. For the quick path, use
[`HOW_TO_VERIFY.md`](./HOW_TO_VERIFY.md) + the root-level `verify.sh`.
This doc covers the manual procedure, the trust model, and the limitations.

Run the first two on every release (each proves
a different thing); the full rebuild is the periodic trust-nothing audit:

| Check                           | Cost      | Proves                                                          |
| ------------------------------- | --------- | --------------------------------------------------------------- |
| **Fast** — digest vs record     | seconds   | The served image == the one CI recorded. Catches re-pointing.   |
| **Signature** — `cosign verify` | seconds   | The image was built/signed by the CI pipeline. Catches forgery. |
| **Full** — rebuild + diff       | minutes\* | Image payload == this source at `<TAG>`. Catches tampering.     |

\* per platform slice; the non-native slice runs under emulation and a Rust
release build there often takes 30+ min.

## Conventions

```bash
IMAGE=gnosischain/bridge-validator
TAG=vX.Y.Z
EXPECTED_SOURCE_COMMIT=<commit the tag must resolve to, obtained out-of-band>
SOURCE_REPO=https://github.com/gnosischain/bridge-validator
```

| Variable           | What it is                               | Source                            |
| ------------------ | ---------------------------------------- | --------------------------------- |
| `$RECORDED_DIGEST` | index digest CI recorded for the release | release **Published image** block |
| `$LIVE_DIGEST`     | index digest the registry serves now     | `imagetools inspect`              |

## What the checks do NOT prove

- That `<TAG>` is a safe/correct release — this is build integrity, not code
  review.
- That the CI runner was uncompromised — identical bytes from the same source
  still pass, and the cosign signature only proves the pipeline signed it, not
  that the pipeline was honest.
- Authenticity of the **tag** — trusted as served by the remote; pin the commit
  with `EXPECTED_SOURCE_COMMIT`. (The digest→builder→repo link IS authenticated
  by the signature check for releases from v0.1.2; the commit↔digest binding in
  the Release body is plaintext, but here it is _also_ cryptographically
  attested — see [Provenance](#appendix--provenance) below.)
- That image IDs / manifest digests match the local rebuild — they never will
  (wall-clock timestamps in the OCI config). The full check compares
  **filesystem content** instead.

---

## Fast check — digest vs. recorded

Confirm the registry serves the exact image CI recorded for the tag.

```bash
# Digest CI recorded (release "Published image" block):
RECORDED_DIGEST=$(gh release view "$TAG" --repo gnosischain/bridge-validator --json body \
  --jq '.body' | grep -oE 'sha256:[0-9a-f]{64}' | head -1)

# Digest the registry serves now (the index digest, covering both platforms):
LIVE_DIGEST=$(docker buildx imagetools inspect "$IMAGE:$TAG" --format '{{.Manifest.Digest}}')

[ "$LIVE_DIGEST" = "$RECORDED_DIGEST" ] && echo match || echo MISMATCH
```

`match` → done. `MISMATCH` → tag re-pointed since release; **stop and
investigate.** No `gh`? Read `digest:` from the release page by eye.

**Trust:** the recorded digest is a plaintext record, not a signature. It
defends against post-publish re-pointing, not a malicious publish. Pair it with
the signature check below; use the full check if you don't trust the pipeline
at all.

---

## Signature check — `cosign verify`

The build pipeline (Docker's `github-builder` reusable workflow) signs the
image's attestation manifests with keyless cosign on every push. Verifying the
signature proves the digest was produced by that workflow running on GitHub
Actions in _this repository_, with the trust anchor in the public Sigstore
transparency log.

Command: [`HOW_TO_VERIFY.md` §2](./HOW_TO_VERIFY.md). Verify against
`$RECORDED_DIGEST`, never the tag.

**Trust:** the certificate identity is the _reusable_ workflow
(`docker/github-builder/.github/workflows/build.yml@...`), which any repository
could call — the binding to _this repo's_ build comes from
`--certificate-github-workflow-repository gnosischain/bridge-validator`. The
signature attests _who built_ the image, not _what is in_ it (that is the full
check), and says nothing about whether the CI runner itself was compromised.
Tags v0.1.1 and older predate the signing pipeline and fail with "no matching
signatures".

---

## Full check — rebuild from source

Independently rebuilds the image and compares the application payload —
`/app`, holding the single `worker` binary and the `migrations` directory —
against what is published. Everything else in the image is base-image content,
already fixed by the digest-pinned `FROM` lines.

This works because the build is deterministic: a digest-pinned builder image,
`cargo build --release --locked` (exact dependency versions from `Cargo.lock`),
a fixed `WORKDIR` (absolute paths embedded in the binary match), and
`SQLX_OFFLINE=true` (no live database influences codegen).

The published image is multi-arch (`linux/amd64` + `linux/arm64`), built on
native runners. Each slice is an independent artifact, so verify both: the
slice matching your host builds natively, the other under emulation (slow).

**Requires:** `docker` (+`buildx`), `git`; a few GB disk; network to Docker Hub
and github.com; emulation for the non-native slice.

### 1. Clean checkout at the tag

```bash
git clone --depth=1 --branch "$TAG" "$SOURCE_REPO" /tmp/bv-src
# Confirm the tag resolves to the commit you trust — else it was re-pointed; stop.
[ "$(git -C /tmp/bv-src rev-parse HEAD)" = "$EXPECTED_SOURCE_COMMIT" ] || echo "TAG MOVED — STOP"
```

### 2. Rebuild one slice with CI's flags

```bash
PLATFORM=linux/amd64   # repeat all steps with linux/arm64
docker buildx build --platform "$PLATFORM" --no-cache --provenance=false --sbom=false --load \
  -f /tmp/bv-src/bridge_validator/Dockerfile -t "bridge-validator:verify-$TAG" /tmp/bv-src
```

`--no-cache` (fresh layers), `--provenance=false --sbom=false` (plain image to
diff), `--load` (import to daemon).

### 3. Pull the published slice, extract `/app` from both

```bash
docker pull --platform "$PLATFORM" "$IMAGE:$TAG"
PUB=$(docker create --platform "$PLATFORM" "$IMAGE:$TAG")
LOC=$(docker create --platform "$PLATFORM" "bridge-validator:verify-$TAG")
docker cp "$PUB:/app" /tmp/bv-pub && docker cp "$LOC:/app" /tmp/bv-loc
docker rm "$PUB" "$LOC"
```

### 4. Diff

```bash
diff -r /tmp/bv-pub /tmp/bv-loc
```

Silent `diff` = **pass**: the `worker` binary and every migration file are
byte-identical for this slice. Repeat from Step 2 for the other platform.

### Interpreting the diff

| Diff result                               | Meaning                                                                                                        |
| ----------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| Silent                                    | ✅ Built from this source.                                                                                     |
| `worker` differs, `migrations/` identical | Toolchain/base drift (wrong builder digest — see [Older tags](#older-tags)) — or tampering. Investigate first. |
| `migrations/` differs                     | Wrong checkout or leaked local changes — re-check `git status` and redo from Step 1.                           |
| Both differ                               | ❌ Likely tampering. Do not deploy; file an issue.                                                             |

### Older tags

Digest-pinned `FROM` lines exist only from the commit that introduced them
onward. Earlier tags had floating `FROM rust:slim-bookworm` /
`FROM debian:bookworm-slim`, so a rebuild today resolves _newer_ bases and the
binary will not reproduce. To verify such a tag, read the base digests CI
resolved from the image's SLSA provenance and pin them locally before Step 2:

```bash
docker buildx imagetools inspect "$IMAGE:$TAG" \
  --format '{{json (index .Provenance "linux/amd64").SLSA.buildDefinition.resolvedDependencies}}' | jq
# then edit the FROM lines in /tmp/bv-src/bridge_validator/Dockerfile to those digests
```

### Cleanup

```bash
rm -rf /tmp/bv-src /tmp/bv-pub /tmp/bv-loc
docker image rm "bridge-validator:verify-$TAG" "$IMAGE:$TAG"
```

---

## Automation — `verify.sh`

`./verify.sh <TAG> <EXPECTED_SOURCE_COMMIT>` (repo root) runs the full check
end-to-end for **both** platforms (clone → rebuild → extract → diff per slice)
and prints a PASS/FAIL summary. `PLATFORMS=linux/amd64` (or `linux/arm64`)
restricts it to one slice.

- **`EXPECTED_SOURCE_COMMIT`** asserts the tag resolves to a commit you
  obtained out-of-band, before the build. Turns "trust the tag" into "trust
  this commit". Without it, an attacker who re-points the tag to a malicious
  commit **and** publishes a matching image passes the diff — the rebuild only
  proves image-matches-its-source, not that the source is the one you trust.
- **Runs on the host, no helper container** — mounting the Docker socket into a
  helper would give it root-equivalent control of the daemon, expanding the
  trust boundary instead of shrinking it.

Exit codes: `0` pass · `1` payload diff under `/app` (possible tampering) ·
`2` setup or tag-commit check failed before the build.

## Limitations & path forward

The full check is content comparison; the _who built it_ gap is covered by the
[signature check](#signature-check--cosign-verify). What still relies on trust:
the tag/commit (pin with `EXPECTED_SOURCE_COMMIT`) and the honesty of the CI
runner (see [What the checks do NOT prove](#what-the-checks-do-not-prove)).
`verify.sh` prints these gaps on every run.

## Appendix — provenance

```bash
docker buildx imagetools inspect --format '{{json .Provenance}}' "$IMAGE:$TAG" | jq
```

| Field                     | Value                                                   |
| ------------------------- | ------------------------------------------------------- |
| `buildType`               | buildkit SLSA                                           |
| `github_repository`       | `gnosischain/bridge-validator`                          |
| `github_sha`              | **the release commit** (see below)                      |
| `github_workflow_ref`     | `…/.github/workflows/release.yml@refs/tags/<TAG>`       |
| `resolvedDependencies[*]` | `rust:slim-bookworm` and `debian:bookworm-slim` digests |

Provenance is keyed per platform in multi-arch images:
`(index .Provenance "linux/amd64").SLSA` (similarly `linux/arm64`).

Because the release workflow triggers on the tag push itself, `github_sha` in
the signed provenance _is_ the release commit. The commit↔digest binding is
therefore cryptographically attested, not merely recorded in the plaintext
Release body — the Release-body record is a convenience copy. You can read it
directly:

```bash
docker buildx imagetools inspect "$IMAGE:$TAG" \
  --format '{{json (index .Provenance "linux/amd64").SLSA}}' \
  | jq -r '.. | .github_sha? // empty' | head -1
```
