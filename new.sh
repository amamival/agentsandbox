# Download nixos/nix image from Docker Hub. Require: curl, tar, jq
set -v
TARGET_DIR="$1"
[[ -z "$TARGET_DIR" ]] && echo "$0 <NEW_SYSROOT>" && exit
mkdir -p "$TARGET_DIR"

REPO=nixos/nix
TAG=latest
ARCH=amd64
OS=linux
REGISTRY_ENDPOINT="https://registry-1.docker.io/v2"

TOKEN="$(curl "https://auth.docker.io/token?service=registry.docker.io&scope=repository:$REPO:pull" | jq -r .token)"
echo TOKEN=$TOKEN
MANIFESTS=$(curl -H "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/manifests/$TAG")
echo MANIFESTS=$MANIFESTS
MANIFEST_DIGEST=$(jq -r ".manifests[] | select(.platform.architecture == \"$ARCH\" and .platform.os == \"$OS\") | .digest" <<<"$MANIFESTS")
echo MANIFEST_DIGEST=$MANIFEST_DIGEST
MANIFEST=$(curl -H "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/manifests/$MANIFEST_DIGEST")
echo MANIFEST=$MANIFEST
BLOBSUMS="$(jq -r '.layers[].digest' <<< "$MANIFEST")"
echo BLOBSUMS=$BLOBSUMS
while read BLOBSUM; do
    curl -IH "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/blobs/$BLOBSUM";
    #curl -LH "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/blobs/$BLOBSUM" > "${BLOBSUM/*:/}.gz";
    curl -LH "Authorization: Bearer $TOKEN" "$REGISTRY_ENDPOINT/$REPO/blobs/$BLOBSUM" \
    | tar zxf - -C "$TARGET_DIR"
done <<<"$BLOBSUMS"
