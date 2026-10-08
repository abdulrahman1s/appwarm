if [ "$(uname -s)" != Linux ] || [ "$(uname -m)" != x86_64 ]; then
  echo "appwarm: prebuilt Niri is available only for x86_64 Linux" >&2
  exit 1
fi

asset="niri-appwarm-v@version@-x86_64-linux.nix-export.zst"
url="https://github.com/abdulrahman1s/appwarm/releases/download/v@version@"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT

curl --fail --location --retry 3 --output "$temp_dir/$asset" "$url/$asset"
curl --fail --location --retry 3 --output "$temp_dir/$asset.sha256" "$url/$asset.sha256"
(cd "$temp_dir" && sha256sum --check "$asset.sha256")
sudo -v
zstd -dc "$temp_dir/$asset" | sudo nix-store --import
