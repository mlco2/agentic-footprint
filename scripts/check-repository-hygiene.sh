#!/bin/sh
set -eu

fail=0

for path in \
  .DS_Store \
  .idea \
  .claude \
  .codex \
  .env \
  site \
  target \
  console/dist \
  console/node_modules
do
  if git ls-files "$path" "$path/" | while IFS= read -r tracked; do
       if [ -e "$tracked" ]; then
         printf '%s\n' "$tracked"
         break
       fi
     done | grep -q .
  then
    echo "repository hygiene: generated or local path is tracked: $path" >&2
    fail=1
  fi
done

if git ls-files | grep -E '(^|/)([^/]*\.(sqlite|sqlite3|db|log)|\.env(\..*)?|id_rsa|id_ed25519)$' >/dev/null; then
  echo "repository hygiene: database, log, environment, or private-key file is tracked" >&2
  git ls-files | grep -E '(^|/)([^/]*\.(sqlite|sqlite3|db|log)|\.env(\..*)?|id_rsa|id_ed25519)$' >&2
  fail=1
fi

if git grep -nIE \
  '(BEGIN (RSA|OPENSSH|EC|PGP) PRIVATE KEY|AKIA[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9]{20,}|xox[baprs]-[A-Za-z0-9-]{10,})' \
  -- ':!Cargo.lock' ':!console/package-lock.json'
then
  echo "repository hygiene: possible credential found in tracked content" >&2
  fail=1
fi

exit "$fail"
