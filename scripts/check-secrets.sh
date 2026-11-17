#!/bin/bash
# Fails if staged changes contain a secret. Needs gitleaks on PATH.
if ! command -v gitleaks >/dev/null 2>&1; then
  echo "check-secrets: gitleaks is not installed. Install it (macOS: brew install gitleaks; Windows: a release binary from https://github.com/gitleaks/gitleaks/releases) and put it on PATH." >&2
  exit 2
fi
gitleaks git --staged --redact
