#!/usr/bin/env bash
# Run on your Mac after exporting a Developer ID Application identity as .p12
# and downloading a team App Store Connect API key as .p8.
set -euo pipefail
set +x

REPO="forserial/skills-manager"
if [[ "${1:-}" == "--help" ]]; then
  echo "Usage: bash scripts/setup-macos-release-secrets.sh"
  echo "Uploads the six Apple signing/notarization secrets to ${REPO}."
  echo "Requires gh authentication, a .p12 identity export, and a team .p8 API key."
  exit 0
fi
command -v gh >/dev/null || { echo "Install GitHub CLI first: brew install gh" >&2; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "Sign in first: gh auth login" >&2; exit 1; }

echo "Configuring Apple release credentials for ${REPO}."
echo "Enter file paths without quotes; ~ at the start is supported."
read -r -p "Developer ID Application .p12 file: " CERT_PATH
read -r -s -p "Password used when exporting the .p12: " CERT_PASSWORD
printf '\n'
read -r -p "Full signing identity (Developer ID Application: Name (TEAMID)): " SIGNING_IDENTITY
read -r -p "App Store Connect team API Issuer ID: " API_ISSUER
read -r -p "App Store Connect API Key ID: " API_KEY
read -r -p "Downloaded AuthKey_KEYID.p8 file: " API_KEY_PATH

CERT_PATH="${CERT_PATH/#\~/$HOME}"
API_KEY_PATH="${API_KEY_PATH/#\~/$HOME}"
[[ -s "$CERT_PATH" ]] || { echo "Certificate file not found or empty." >&2; exit 1; }
[[ -s "$API_KEY_PATH" ]] || { echo "API key file not found or empty." >&2; exit 1; }
[[ -n "$CERT_PASSWORD" ]] || { echo "Export the .p12 with a nonempty password." >&2; exit 1; }
[[ "$SIGNING_IDENTITY" == "Developer ID Application: "* ]] || {
  echo "Use a Developer ID Application identity, as listed by security find-identity." >&2
  exit 1
}
[[ "$API_ISSUER" =~ ^[[:xdigit:]]{8}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{12}$ ]] || {
  echo "Issuer ID must be the UUID from App Store Connect, not your Team ID." >&2
  exit 1
}
[[ "$API_KEY" =~ ^[A-Z0-9]{10}$ ]] || { echo "Expected a 10-character API Key ID." >&2; exit 1; }
grep -q -- '-----BEGIN PRIVATE KEY-----' "$API_KEY_PATH" || {
  echo "The .p8 file does not contain the expected PEM private key." >&2
  exit 1
}

# Values go to gh over stdin, never through command-line arguments.
# Keep shell tracing disabled: it would expose credentials.
set +x
base64 < "$CERT_PATH" | tr -d '\r\n' | gh secret set APPLE_CERTIFICATE --repo "$REPO"
printf '%s' "$CERT_PASSWORD" | gh secret set APPLE_CERTIFICATE_PASSWORD --repo "$REPO"
unset CERT_PASSWORD
printf '%s' "$SIGNING_IDENTITY" | gh secret set APPLE_SIGNING_IDENTITY --repo "$REPO"
printf '%s' "$API_ISSUER" | gh secret set APPLE_API_ISSUER --repo "$REPO"
printf '%s' "$API_KEY" | gh secret set APPLE_API_KEY --repo "$REPO"
base64 < "$API_KEY_PATH" | tr -d '\r\n' | gh secret set APPLE_API_KEY_BASE64 --repo "$REPO"

echo "Uploaded all six Apple secrets. Secret names only:"
gh secret list --repo "$REPO"
