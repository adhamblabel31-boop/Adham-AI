#!/bin/sh
set -eu
umask 022

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 /path/to/verified-upstream-release-directory" >&2
    exit 2
fi

release_dir=$(CDPATH= cd -- "$1" && pwd)
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
package_count=0

for candidate in "$release_dir"/*.deb; do
    if [ -f "$candidate" ]; then
        package=$candidate
        package_count=$((package_count + 1))
    fi
done
if [ "$package_count" -ne 1 ]; then
    echo "Expected exactly one Debian package in the release directory." >&2
    exit 1
fi

manifest="$release_dir/SHA256SUMS"
signature="$release_dir/SHA256SUMS.sig"
public_hex="$repo_root/adham-ai-update-public-key.hex"
test -s "$manifest"
test -s "$signature"
test -s "$public_hex"
test "$(wc -c < "$signature" | tr -d ' ')" -eq 64

package_name=$(basename -- "$package")
package_version=$(dpkg-deb --field "$package" Version)
test "$package_name" = "adham-ai_${package_version}_amd64.deb"
test "$(dpkg-deb --field "$package" Package)" = adham-ai
test "$(dpkg-deb --field "$package" Architecture)" = amd64
test "$(dpkg-deb --field "$package" Depends | tr -d ' ' | grep -c 'libc6(>=2\.43)')" -eq 1

expected_checksum_line=$(awk -v name="$package_name" '$2 == name { print; count++ } END { if (count != 1) exit 1 }' "$manifest")
test -n "$expected_checksum_line"
(cd "$release_dir" && sha256sum --check SHA256SUMS)

tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/adham-ai-apt-publish.XXXXXX")
cleanup() {
    rm -rf -- "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

python3 - "$public_hex" "$tmp_dir/update-public.der" <<'PY'
import pathlib
import sys

raw = bytes.fromhex(pathlib.Path(sys.argv[1]).read_text().strip())
if len(raw) != 32:
    raise SystemExit("The trusted application Ed25519 public key must be 32 bytes.")
pathlib.Path(sys.argv[2]).write_bytes(bytes.fromhex("302a300506032b6570032100") + raw)
PY
openssl pkeyutl -verify -rawin -pubin -keyform DER \
    -inkey "$tmp_dir/update-public.der" \
    -in "$manifest" -sigfile "$signature" >/dev/null

key_home=${ADHAM_AI_APT_GNUPGHOME:-"$HOME/.local/share/adham-ai-apt-signing/gnupg"}
passphrase_file=${ADHAM_AI_APT_PASSPHRASE_FILE:-"$HOME/.config/adham-ai/apt-signing/key-passphrase"}
fingerprint=$(sed -n 's/^Fingerprint: *//p' "$repo_root/apt-archive-key-fingerprint.txt")
test -n "$fingerprint"
test -d "$key_home"
test "$(stat -c '%a' "$key_home")" = 700
test -f "$passphrase_file"
test "$(stat -c '%a' "$passphrase_file")" = 600

destination="$repo_root/pool/main/a/adham-ai/$package_name"
mkdir -p "$(dirname -- "$destination")"
if [ -e "$destination" ]; then
    if ! cmp -s "$package" "$destination"; then
        echo "Refusing to overwrite an existing package version with different bytes." >&2
        exit 1
    fi
else
    install -m 0644 "$package" "$destination"
fi

index_dir="$repo_root/dists/resolute/main/binary-amd64"
mkdir -p "$index_dir"
(cd "$repo_root" && dpkg-scanpackages --arch amd64 pool /dev/null) > "$tmp_dir/Packages.raw"
python3 - "$tmp_dir/Packages.raw" "$tmp_dir/Packages" <<'PY'
import pathlib
import sys

data = pathlib.Path(sys.argv[1]).read_bytes()
pathlib.Path(sys.argv[2]).write_bytes(data.rstrip(b"\n") + b"\n")
PY
install -m 0644 "$tmp_dir/Packages" "$index_dir/Packages"
gzip -n -9 -c "$tmp_dir/Packages" > "$tmp_dir/Packages.gz"
install -m 0644 "$tmp_dir/Packages.gz" "$index_dir/Packages.gz"

python3 - "$repo_root" "$tmp_dir/Release" <<'PY'
import datetime
import hashlib
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
release = root / "dists/resolute"
entries = []
for relative in (
    "main/binary-amd64/Packages",
    "main/binary-amd64/Packages.gz",
):
    data = (release / relative).read_bytes()
    entries.append((hashlib.sha256(data).hexdigest(), len(data), relative))
date = datetime.datetime.now(datetime.timezone.utc).strftime("%a, %d %b %Y %H:%M:%S UTC")
lines = [
    "Origin: Adham AI",
    "Label: Adham AI",
    "Suite: resolute",
    "Codename: resolute",
    "Date: " + date,
    "Architectures: amd64",
    "Components: main",
    "Description: Adham AI packages for Ubuntu 26.04 LTS AMD64",
    "SHA256:",
]
for digest, size, name in entries:
    lines.append(f" {digest} {size:16d} {name}")
pathlib.Path(sys.argv[2]).write_text("\n".join(lines) + "\n", encoding="ascii")
PY
install -m 0644 "$tmp_dir/Release" "$repo_root/dists/resolute/Release"

gpg --homedir "$key_home" --batch --pinentry-mode loopback \
    --passphrase-file "$passphrase_file" --local-user "$fingerprint" \
    --digest-algo SHA256 --clearsign \
    --output "$tmp_dir/InRelease" "$tmp_dir/Release"
gpg --homedir "$key_home" --batch --pinentry-mode loopback \
    --passphrase-file "$passphrase_file" --local-user "$fingerprint" \
    --digest-algo SHA256 --detach-sign \
    --output "$tmp_dir/Release.gpg" "$tmp_dir/Release"

gpg --homedir "$key_home" --batch --verify "$tmp_dir/InRelease" >/dev/null
gpg --homedir "$key_home" --batch --verify "$tmp_dir/Release.gpg" "$tmp_dir/Release" >/dev/null
gpg --homedir "$key_home" --batch --output "$tmp_dir/exported-key.gpg" --export "$fingerprint"
cmp -s "$tmp_dir/exported-key.gpg" "$repo_root/apt-archive-keyring.gpg"

install -m 0644 "$tmp_dir/InRelease" "$repo_root/dists/resolute/InRelease"
install -m 0644 "$tmp_dir/Release.gpg" "$repo_root/dists/resolute/Release.gpg"

echo "Published repository metadata for $package_name"
echo "Upstream Ed25519 signature, package checksum, and APT metadata signatures verified."
