#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "Usage: CONFIRM_RESTORE=inkstone scripts/restore-postgres.sh BACKUP.dump" >&2
    exit 2
fi
if [[ "${CONFIRM_RESTORE:-}" != "inkstone" ]]; then
    echo "Restore replaces the current database; set CONFIRM_RESTORE=inkstone to continue" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ENV_FILE="${ENV_FILE:-$PROJECT_DIR/.env.production}"
backup="$1"

if [[ ! -f "$ENV_FILE" ]]; then
    echo "Environment file not found: $ENV_FILE" >&2
    exit 1
fi
if [[ ! -f "$backup" ]]; then
    echo "Backup file not found: $backup" >&2
    exit 1
fi

if ! docker compose --project-directory "$PROJECT_DIR" --env-file "$ENV_FILE" \
    -f "$PROJECT_DIR/docker-compose.yml" exec -T postgres pg_restore --list \
    < "$backup" >/dev/null; then
    echo "Backup validation failed; the server was not stopped" >&2
    exit 1
fi

temporary_db="inkstone_restore_$(date -u +%Y%m%d%H%M%S)_$$"
rollback_db="inkstone_rollback_$(date -u +%Y%m%d%H%M%S)_$$"
cleanup_temporary_database() {
    docker compose --project-directory "$PROJECT_DIR" --env-file "$ENV_FILE" \
        -f "$PROJECT_DIR/docker-compose.yml" exec -T -e RESTORE_DB="$temporary_db" postgres \
        sh -eu -c 'dropdb --if-exists --force --username="$POSTGRES_USER" "$RESTORE_DB"' \
        >/dev/null 2>&1 || true
}
trap cleanup_temporary_database EXIT

docker compose --project-directory "$PROJECT_DIR" --env-file "$ENV_FILE" \
    -f "$PROJECT_DIR/docker-compose.yml" exec -T -e RESTORE_DB="$temporary_db" postgres \
    sh -eu -c 'createdb --template=template0 --username="$POSTGRES_USER" "$RESTORE_DB"'
if ! docker compose --project-directory "$PROJECT_DIR" --env-file "$ENV_FILE" \
    -f "$PROJECT_DIR/docker-compose.yml" exec -T -e RESTORE_DB="$temporary_db" postgres \
    sh -eu -c 'pg_restore --exit-on-error --no-owner --no-privileges --username="$POSTGRES_USER" --dbname="$RESTORE_DB"' \
    < "$backup"; then
    echo "Restore validation failed; the original database is unchanged" >&2
    exit 1
fi

trap - EXIT
docker compose --project-directory "$PROJECT_DIR" --env-file "$ENV_FILE" \
    -f "$PROJECT_DIR/docker-compose.yml" stop server
if ! docker compose --project-directory "$PROJECT_DIR" --env-file "$ENV_FILE" \
    -f "$PROJECT_DIR/docker-compose.yml" exec -T \
    -e RESTORE_DB="$temporary_db" -e ROLLBACK_DB="$rollback_db" postgres \
    sh -eu -c '
        case "$POSTGRES_DB:$RESTORE_DB:$ROLLBACK_DB" in
            *[!A-Za-z0-9_:]*) echo "Unsafe database name" >&2; exit 1 ;;
        esac
        psql --username="$POSTGRES_USER" --dbname=postgres --set=ON_ERROR_STOP=1 \
            --command="SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname IN ('\''$POSTGRES_DB'\'', '\''$RESTORE_DB'\'') AND pid <> pg_backend_pid()"
        psql --username="$POSTGRES_USER" --dbname=postgres --set=ON_ERROR_STOP=1 \
            --command="BEGIN; ALTER DATABASE \"$POSTGRES_DB\" RENAME TO \"$ROLLBACK_DB\"; ALTER DATABASE \"$RESTORE_DB\" RENAME TO \"$POSTGRES_DB\"; COMMIT"
    '; then
    echo "Database swap failed; the original database is unchanged, the validated database $temporary_db was preserved, and the server remains stopped" >&2
    exit 1
fi

if ! docker compose --project-directory "$PROJECT_DIR" --env-file "$ENV_FILE" \
    -f "$PROJECT_DIR/docker-compose.yml" start --wait server; then
    docker compose --project-directory "$PROJECT_DIR" --env-file "$ENV_FILE" \
        -f "$PROJECT_DIR/docker-compose.yml" stop server >/dev/null 2>&1 || true
    if docker compose --project-directory "$PROJECT_DIR" --env-file "$ENV_FILE" \
        -f "$PROJECT_DIR/docker-compose.yml" exec -T \
        -e RESTORE_DB="$temporary_db" -e ROLLBACK_DB="$rollback_db" postgres \
        sh -eu -c '
            case "$POSTGRES_DB:$RESTORE_DB:$ROLLBACK_DB" in
                *[!A-Za-z0-9_:]*) echo "Unsafe database name" >&2; exit 1 ;;
            esac
            psql --username="$POSTGRES_USER" --dbname=postgres --set=ON_ERROR_STOP=1 \
                --command="SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname IN ('\''$POSTGRES_DB'\'', '\''$ROLLBACK_DB'\'') AND pid <> pg_backend_pid()"
            psql --username="$POSTGRES_USER" --dbname=postgres --set=ON_ERROR_STOP=1 \
                --command="BEGIN; ALTER DATABASE \"$POSTGRES_DB\" RENAME TO \"$RESTORE_DB\"; ALTER DATABASE \"$ROLLBACK_DB\" RENAME TO \"$POSTGRES_DB\"; COMMIT"
        '; then
        docker compose --project-directory "$PROJECT_DIR" --env-file "$ENV_FILE" \
            -f "$PROJECT_DIR/docker-compose.yml" start --wait server >/dev/null 2>&1 || true
        echo "Restored server failed health checks; the original database was restored and the validated database remains $temporary_db" >&2
    else
        echo "Restored server failed health checks and rollback failed; the previous database remains $rollback_db" >&2
    fi
    exit 1
fi
if ! docker compose --project-directory "$PROJECT_DIR" --env-file "$ENV_FILE" \
    -f "$PROJECT_DIR/docker-compose.yml" exec -T -e ROLLBACK_DB="$rollback_db" postgres \
    sh -eu -c 'dropdb --if-exists --force --username="$POSTGRES_USER" "$ROLLBACK_DB"'; then
    echo "Warning: restore succeeded, but previous database $rollback_db could not be removed" >&2
fi
echo "Restore completed from $backup"
