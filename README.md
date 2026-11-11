# Adham AI APT Repository

This repository publishes signed Debian packages for **Ubuntu 26.04 LTS
(Resolute), AMD64 only**, with glibc 2.43 or later.

- APT base URL: <https://adhamblabel31-boop.github.io/Adham-AI-APT/>
- Distribution: `resolute`
- Component: `main`
- Architecture: `amd64`
- Package: `adham-ai`

APT metadata is signed with a dedicated RSA archive key. This key is separate
from the Ed25519 key used by the application to authenticate upstream release
checksums. Only the public archive key is published here. The private key and
its passphrase remain outside this repository.

## Install

Review the archive key fingerprint in
[`apt-archive-key-fingerprint.txt`](./apt-archive-key-fingerprint.txt). It must
match this independently published value:

```text
7265 BB14 C256 8E99 6CD5 7B2D 6A02 2D3F 14BD 84DB
```

Install the dedicated keyring and source entry. This does not install Adham AI;
it only configures the APT source.

```sh
set -eu
repo='https://adhamblabel31-boop.github.io/Adham-AI-APT'
tmp_key="$(mktemp)"
trap 'rm -f "$tmp_key"' EXIT

curl --fail --silent --show-error --location \
  "$repo/apt-archive-keyring.gpg" -o "$tmp_key"
fingerprint="$(gpg --show-keys --with-colons "$tmp_key" |
  awk -F: '$1 == "fpr" { print $10; exit }')"
test "$fingerprint" = '7265BB14C2568E996CD57B2D6A022D3F14BD84DB'

sudo install -o root -g root -m 0644 "$tmp_key" \
  /usr/share/keyrings/adham-ai-archive-keyring.gpg

cat <<'EOF' | sudo tee /etc/apt/sources.list.d/adham-ai.sources >/dev/null
Types: deb
URIs: https://adhamblabel31-boop.github.io/Adham-AI-APT
Suites: resolute
Components: main
Architectures: amd64
Signed-By: /usr/share/keyrings/adham-ai-archive-keyring.gpg
EOF
```

After reviewing the source entry, refresh package metadata:

```sh
sudo apt update
```

To inspect the candidate without installing it:

```sh
apt-cache policy adham-ai
```

Install or upgrade only when you explicitly choose to:

```sh
sudo apt install adham-ai
```

No commands in this repository automatically install or upgrade the package.

## Repository layout

```text
apt-archive-keyring.gpg
apt-archive-key-fingerprint.txt
dists/resolute/Release
dists/resolute/InRelease
dists/resolute/Release.gpg
dists/resolute/main/binary-amd64/Packages
dists/resolute/main/binary-amd64/Packages.gz
pool/main/a/adham-ai/adham-ai_<version>_amd64.deb
```

## Publishing a later release

On the trusted Ubuntu 26.04 AMD64 publishing machine:

1. Download the `.deb`, `SHA256SUMS`, and `SHA256SUMS.sig` from the official
   Adham AI GitHub Release.
2. Verify the detached Ed25519 signature over the exact checksum manifest
   using the trusted application update public key.
3. Verify `sha256sum --check SHA256SUMS`.
4. Run `scripts/publish-release.sh /path/to/verified/release-directory`.
   The script rechecks the Ed25519 signature, checksum, package name, version,
   architecture, and glibc dependency before adding the package. It regenerates
   `Packages`, `Packages.gz`, `Release`, `InRelease`, and `Release.gpg`, and
   verifies the signed metadata locally.
5. Review the diff, commit, and push to `main`. GitHub Pages serves the root
   of `main`; no signing key or passphrase is needed by GitHub Actions.
6. Validate the public HTTPS repository and `apt update` before announcing it.

The local signing key is stored under
`~/.local/share/adham-ai-apt-signing/gnupg`. Its passphrase is stored separately
at `~/.config/adham-ai/apt-signing/key-passphrase`; both locations are local,
owner-only, and must never be copied into this repository, an Actions secret,
or a release artifact. Back up these files securely and rotate the public key
deliberately if they are lost or compromised.
