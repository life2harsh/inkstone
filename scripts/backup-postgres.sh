#!/usr/bin/env bash
set -euo pipefail
umask 077

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ENV_FILE="${ENV_FILE:-$PROJECT_DIR/.env.production}"
BACKUP_DIR="${BACKUP_DIR:-$PROJECT_DIR/backups}"
BACKUP_RETENTION_DAYS="${BACKUP_RETENTION_DAYS:-14}"

if [[ ! -f "$ENV_FILE" ]]; then
    echo "Environment file not found: $ENV_FILE" >&2
    exit 1
fi
if [[ ! "$BACKUP_RETENTION_DAYS" =~ ^[0-9]+$ ]]; then
    echo "BACKUP_RETENTION_DAYS must be a non-negative integer" >&2
    exit 1
fi

mkdir -p "$BACKUP_DIR"
chmod 700 "$BACKUP_DIR"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
temporary_backup="$(mktemp "$BACKUP_DIR/.inkstone-$timestamp-XXXXXXXX.dump.tmp")"
temporary_name="${temporary_backup##*/}"
backup="$BACKUP_DIR/${temporary_name#.}"
backup="${backup%.tmp}"
trap 'rm -f "$temporary_backup"' EXIT

docker compose --project-directory "$PROJECT_DIR" --env-file "$ENV_FILE" \
    -f "$PROJECT_DIR/docker-compose.yml" exec -T postgres \
    sh -eu -c 'pg_dump --format=custom --no-owner --no-privileges --username="$POSTGRES_USER" --dbname="$POSTGRES_DB"' \
    > "$temporary_backup"
mv --no-clobber "$temporary_backup" "$backup"
if [[ -e "$temporary_backup" ]]; then
    echo "Backup destination already exists: $backup" >&2
    exit 1
fi
chmod 600 "$backup"
trap - EXIT

find "$BACKUP_DIR" -maxdepth 1 -type f -name 'inkstone-*.dump' \
    -mtime "+$BACKUP_RETENTION_DAYS" -delete
echo "Backup written to $backup"
