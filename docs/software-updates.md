# Software Updates

## Current behavior

The Settings page can manually check the confirmed GitHub repository for the
highest stable SemVer release. Checks are asynchronous and do not delay
application startup. After a successful check, users can explicitly download
an update. The application downloads `SHA256SUMS` and its detached signature,
verifies the signature with the configured key, then downloads the Debian
package and validates its SHA-256 digest before marking it ready. Download
progress and cancellation are available. Updates require a separate user
confirmation and PolicyKit authorization before installation. The application
does not perform automatic update checks.

The trusted Ed25519 public key is configured at build time as
`ADHAM_AI_UPDATE_PUBLIC_KEY` in `.cargo/config.toml` (64 hexadecimal
characters / 32 bytes). This is the public key supplied by the project owner;
its encoding and Ed25519 point strength were validated. Only the public key is
embedded in the application. If the variable is missing, malformed, or weak,
verified downloads remain disabled. Never provide the private signing key to
the application, repository, build environment, or release workflow.

Downloading requires an explicit user action. Installing requires a second,
separate confirmation in Settings, followed by PolicyKit authorization.

## Supported platform for the first release

The first Adham AI release targets **Ubuntu 26.04 LTS on AMD64 (x86_64) only**.
The packaged binary requires glibc 2.43 or later; the Debian package declares
the automatically detected `libc6 (>= 2.43)` dependency. Other distributions,
Ubuntu releases, and CPU architectures are not supported for this first
release, even if they may happen to satisfy the glibc requirement.

Build and validate the release package on Ubuntu 26.04 AMD64. The Debian
package version must exactly match the SemVer release tag, with no Debian
revision suffix, because update verification compares the installed package
version and release version exactly. `Cargo.toml` sets cargo-deb's `revision`
to an empty string for this reason. The release asset name is
`adham-ai_<version>_amd64.deb`.

## GitHub release format

Publish stable releases with a SemVer tag (for example `v1.2.3`) and these
assets:

- `adham-ai_<version>_amd64.deb`, for example `adham-ai_1.2.3_amd64.deb`
- `SHA256SUMS`, containing standard two-space-separated SHA-256 entries
- `SHA256SUMS.sig`, a detached Ed25519 signature over the exact bytes of
  `SHA256SUMS` (64 raw bytes)

Release discovery selects the highest non-draft stable version and rejects
prereleases, unsupported architectures, missing/duplicate assets, unsafe
asset URLs, and oversized files. The download flow verifies the signature
before trusting the checksum manifest, then checks the downloaded package's
archive header and SHA-256 digest. Failed, cancelled, and interrupted
downloads are not presented as verified packages.

Release notes and the publication date are read from the GitHub Releases API.
Do not publish a release until the asset names, package version, signature,
and checksum manifest have been independently verified.

## Preparing and signing release checksums

Build the package on Ubuntu 26.04 AMD64, then create `SHA256SUMS` with the
exact release asset filename:

```sh
sha256sum adham-ai_<version>_amd64.deb > SHA256SUMS
```

Keep the existing Ed25519 private release key offline and outside the
repository and build workspace. On the isolated signing machine, sign the
manifest's exact bytes with OpenSSL 3:

```sh
openssl pkeyutl -sign -rawin \
  -inkey /absolute/path/to/offline-ed25519-private-key.pem \
  -in SHA256SUMS \
  -out SHA256SUMS.sig
```

The detached signature must be 64 raw bytes. Do not place the key path's
private-key file in the release workspace, and never commit, upload, or log
the private key. Before publishing, verify the signature against the trusted
public key and independently verify the package digest against the signed
manifest. Only `SHA256SUMS` and `SHA256SUMS.sig` are release assets; the
private key is not.

## Key management

- Generate and store the private signing key outside this repository and the
  application build environment.
- Keep backups and access to the private key under a separate secure process.
- Only the public key may be provided to the application, after its
  fingerprint and provenance have been verified.
- Configure the verified public key at build time using
  `ADHAM_AI_UPDATE_PUBLIC_KEY`. This repository currently sets that variable
  in `.cargo/config.toml`.
- Key rotation requires a deliberate trust transition. Do not replace a
  configured key or accept a key from a release or checksum manifest.

## Manual updates

1. Open the official [Adham AI Releases page](https://github.com/adhamblabel31-boop/Adham-AI/releases).
2. Select the stable Ubuntu 26.04 LTS AMD64 release and download its `.deb`,
   `SHA256SUMS`, and `SHA256SUMS.sig` assets.
3. Verify `SHA256SUMS.sig` using the independently verified trusted public
   key, then verify the `.deb` digest against the signed manifest.
4. Open the verified `.deb` in Ubuntu's package installer and review its
   confirmation prompt before installing.

The in-app workflow requires separate user confirmation and PolicyKit
authorization before installation. Never install a package whose signature or
checksum cannot be verified.

## In-app installation and data safety

The installer accepts only a `VerifiedPackage` produced by the signed download
workflow. Before installation, it recomputes the package SHA-256 digest and
checks the Debian control metadata for package name `adham-ai`, the expected
release version, and `amd64` architecture.

After explicit confirmation, the application invokes `dpkg-deb` to inspect
metadata, then uses PolicyKit to copy the package into APT's root-owned,
non-writable archive directory. It verifies the staged copy's ownership,
permissions, metadata, and SHA-256 digest again before invoking APT on that
protected copy. This prevents a user-writable download from being swapped
between verification and privileged installation. The copy and APT operation
are separate PolicyKit commands, and both are invoked with distinct process
arguments; no shell is used. The application does not execute the downloaded
file or run the main application as root. APT is called with `install
--assume-yes --no-remove`; `--no-remove` prevents it from removing installed
packages.

The application reports package-manager locks, dependency failures, PolicyKit
authorization denial, authorization-dialog dismissal, and other installation
failures separately. The user can cancel the in-app confirmation or dismiss
PolicyKit authorization. Once APT has started changing system packages, the
application does not forcibly terminate it because doing so could leave the
package database inconsistent. The protected staging copy remains in APT's
archive cache and can be removed with the system package manager's normal
cache-cleaning tools.

After a successful installation, Settings offers a separate **Restart Adham
AI** action. The application does not restart or close itself automatically.
The package upgrade does not reset or modify chat history, application
settings, documents, databases, generated files, Ollama configuration, or
ComfyUI configuration. Automated tests use a mock process runner and never
perform a real system installation.

Automatic installation remains unavailable. Each installation is initiated
only by the user's explicit Settings confirmation and PolicyKit authorization.
