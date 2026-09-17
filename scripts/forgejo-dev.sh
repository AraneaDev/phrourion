#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

URL=http://localhost:3000
CONTAINER=phrourion-forgejo
ADMIN_USER=phro-admin
ADMIN_PASS=phro-admin-pw-12345
ADMIN_EMAIL=admin@phrourion.test
REPO=phro-test-repo

docker compose -f docker-compose.forgejo.yml up -d

echo "Waiting for Forgejo to respond..." >&2
until curl -sf "$URL/api/healthz" >/dev/null 2>&1; do
  sleep 1
done

# docker exec defaults to the image's root user, but the forgejo CLI refuses
# to run as root; the entrypoint's own commands run as the container's git
# user (uid 1000), so we do the same via --user.
if ! docker exec --user 1000 "$CONTAINER" forgejo admin user list 2>/dev/null | grep -q "$ADMIN_USER"; then
  docker exec --user 1000 "$CONTAINER" forgejo admin user create \
    --username "$ADMIN_USER" --password "$ADMIN_PASS" \
    --email "$ADMIN_EMAIL" --admin --must-change-password=false
fi

# Token names must be unique per user, so a re-run against an already-seeded
# instance would otherwise fail with "access token name has been used
# already". Delete any leftover token from a previous run first.
curl -sf -u "$ADMIN_USER:$ADMIN_PASS" -X DELETE \
  "$URL/api/v1/users/$ADMIN_USER/tokens/phrourion-dev" >/dev/null 2>&1 || true

TOKEN=$(curl -sf -u "$ADMIN_USER:$ADMIN_PASS" -X POST \
  "$URL/api/v1/users/$ADMIN_USER/tokens" \
  -H "Content-Type: application/json" \
  -d '{"name":"phrourion-dev","scopes":["write:repository","write:issue"]}' \
  | jq -r '.sha1')

curl -sf -u "$ADMIN_USER:$ADMIN_PASS" -X POST "$URL/api/v1/user/repos" \
  -H "Content-Type: application/json" \
  -d "{\"name\":\"$REPO\",\"auto_init\":true}" >/dev/null 2>&1 || true

CONTENT=$(printf 'hello from the topic branch' | base64 -w0)
curl -sf -u "$ADMIN_USER:$ADMIN_PASS" -X POST \
  "$URL/api/v1/repos/$ADMIN_USER/$REPO/contents/topic.txt" \
  -H "Content-Type: application/json" \
  -d "{\"content\":\"$CONTENT\",\"message\":\"feat: topic\",\"branch\":\"main\",\"new_branch\":\"topic\"}" \
  >/dev/null 2>&1 || true

curl -sf -u "$ADMIN_USER:$ADMIN_PASS" -X POST \
  "$URL/api/v1/repos/$ADMIN_USER/$REPO/pulls" \
  -H "Content-Type: application/json" \
  -d '{"title":"feat: topic","head":"topic","base":"main"}' \
  >/dev/null 2>&1 || true

curl -sf -u "$ADMIN_USER:$ADMIN_PASS" -X POST \
  "$URL/api/v1/repos/$ADMIN_USER/$REPO/issues" \
  -H "Content-Type: application/json" \
  -d '{"title":"bug: something broke"}' \
  >/dev/null 2>&1 || true

echo ""
echo "Forgejo is up. Run:"
echo "  export PHROURION_FORGEJO_TEST_URL=$URL"
echo "  export PHROURION_TOKEN_LOCALHOST=$TOKEN"
echo ""
echo "Test repo identity for fixtures: host=\"localhost\" project=\"$ADMIN_USER/$REPO\""
