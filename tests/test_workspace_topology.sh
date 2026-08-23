#!/usr/bin/env bash
set -euo pipefail

METADATA=$(cargo metadata --no-deps --format-version 1)

# 1. Exact package names in workspace metadata
PACKAGES=$(echo "$METADATA" | jq -r '.packages[].name' | sort | uniq)
PACKAGE_COUNT=$(echo "$PACKAGES" | wc -l)

if [ "$PACKAGE_COUNT" -ne 9 ]; then
  echo "ERROR: Expected 9 workspace packages, found $PACKAGE_COUNT:"
  echo "$PACKAGES"
  exit 1
fi

EXPECTED_PACKAGES=(
  "shilpo"
  "shilpo-device"
  "shilpo-domain"
  "shilpo-ext-api"
  "shilpo-ext-runtime"
  "shilpo-observability"
  "shilpo-registry-contract"
  "shilpo-services"
  "shilpo-theme-daemon"
)

for pkg in "${EXPECTED_PACKAGES[@]}"; do
  if ! echo "$PACKAGES" | grep -q "^$pkg$"; then
    echo "ERROR: Missing expected package $pkg"
    exit 1
  fi
done

# 2. No core/* -> desktop/* dependency edge
CORE_DEPS_DESKTOP=$(echo "$METADATA" | jq -r '.packages[] | select(.manifest_path | contains("/core/")) | .dependencies[].name' | grep '^shilpo$\|^shilpo-services\|^shilpo-device\|^shilpo-ext-runtime\|^shilpo-theme-daemon' || true)

if [ -n "$CORE_DEPS_DESKTOP" ]; then
  echo "ERROR: core/* package depends on desktop/* package:"
  echo "$CORE_DEPS_DESKTOP"
  exit 1
fi

# 3. shilpo-device has no UI or Services dependency
DEVICE_DEPS=$(echo "$METADATA" | jq -r '.packages[] | select(.name == "shilpo-device") | .dependencies[] | select(.kind == null or .kind == "normal") | .name')
if echo "$DEVICE_DEPS" | grep -q 'shilpo-services\|shilpo-m3e\|shilpo$'; then
  echo "ERROR: shilpo-device depends on UI/Services/Shilpo:"
  echo "$DEVICE_DEPS"
  exit 1
fi

# 4. Exactly one desktop product binary named shilpo
PRODUCT_BINS=$(echo "$METADATA" | jq -r '.packages[] | select(.manifest_path | contains("/desktop/")) | .targets[] | select(.kind == ["bin"]) | .name' | grep -v '^generate_' | sort | uniq)

if [ "$PRODUCT_BINS" != "shilpo" ]; then
  echo "ERROR: Expected exactly one desktop product binary named 'shilpo', found:"
  echo "$PRODUCT_BINS"
  exit 1
fi

echo "All workspace topology invariants passed cleanly!"
